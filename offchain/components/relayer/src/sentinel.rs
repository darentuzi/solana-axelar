use std::time::Duration;

use gmp_gateway::events::{CallContract, GatewayEvent};
use solana_program::pubkey::Pubkey;
use solana_sdk::signature::Signature;
use tokio::pin;
use tokio::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use url::Url;

use self::transaction_scanner::transaction_retriever::TransactionRetrieverError;
use self::types::TransactionScannerMessage;
use crate::amplifier_api;
use crate::config::SOLANA_CHAIN_NAME;
use crate::sentinel::error::SentinelError;
use crate::sentinel::transaction_scanner::TransactionScanner;
use crate::sentinel::types::SolanaTransaction;
use crate::sentinel::types::TransactionScannerMessage::{Message, Terminated};
use crate::state::State;
use crate::transports::SolanaToAxelarMessage;

mod error;
mod transaction_scanner;
mod types;

// TODO: All those contants should be configurable
const FETCH_SIGNATURES_INTERVAL: Duration = Duration::from_secs(5);

/// Solana Sentinel
///
/// Monitors the Solana Gateway program for relevant events.
pub struct SolanaSentinel {
    gateway_address: Pubkey,
    rpc: Url,
    verifier_channel: Sender<SolanaToAxelarMessage>,
    state: State,
    cancellation_token: CancellationToken,
}

impl SolanaSentinel {
    pub fn new(
        gateway_address: Pubkey,
        rpc: Url,
        verifier_channel: Sender<SolanaToAxelarMessage>,
        state: State,
        cancellation_token: CancellationToken,
    ) -> Self {
        Self {
            gateway_address,
            rpc,
            verifier_channel,
            state,
            cancellation_token,
        }
    }

    /// Tries to run [`SolanaSentinel::work`] forever.
    /// If it ever returns, signal a cancellation request to all descendant
    /// tasks and wait for them to finish before returning.
    #[tracing::instrument(name = "solana-sentinel", skip(self))]
    pub async fn run(self) {
        info!("task started");

        // Keep a copy of the root cancelation token before it is moved.
        let cancellation_token = self.cancellation_token.clone();

        tokio::select! {
            _ = cancellation_token.cancelled() => {
                warn!("cancellation signal received")
            }
            error = self.work() => {
                // Sentinel task should run forever, but it returned with some error.
                match error {
                    Ok(()) => warn!("worker returned without errors"),
                    Err(sentinel_error) => error!(%sentinel_error),
                }
            }
        };
    }

    /// Listens to Gateway program logs and forward them to the Axelar Verifier
    /// worker.
    #[tracing::instrument(skip_all, err)]
    async fn work(self) -> Result<(), SentinelError> {
        let (transaction_scanner_future, mut transaction_receiver) = TransactionScanner::setup(
            self.gateway_address,
            self.state.clone(),
            self.rpc.clone(),
            FETCH_SIGNATURES_INTERVAL,
            self.cancellation_token.child_token(),
        );
        pin!(transaction_scanner_future);

        // Cancelation routine
        let cleanup = |error: SentinelError| -> Result<(), SentinelError> {
            self.cancellation_token.cancel();
            Err(error)
        };

        // Listens for incoming Solana transactions and process them sequentially to
        // propperly update the latest known transaction signature.
        loop {
            // Handling the message within the `tokio::select` scope triggers a compilation
            // error suggesting the future isn't `Send`. To address this, we
            // assign it to a variable and handle it outside of the
            // macro's body.
            let optional_message = tokio::select! {
                _ = self.cancellation_token.cancelled() => { return cleanup(SentinelError::Stopped); }

                // Advance the TransactionScanner future
                _ = &mut transaction_scanner_future => { continue }

                // TODO: use recv_many() to increase throughput and register the latest known signature only once per call.
                message = transaction_receiver.recv() => { message }
            };

            // Unpack the message
            let Some(message) = optional_message else {
                warn!(
                    reason = "transaction scanner channel was closed",
                    "emitting cancel signal"
                );
                return cleanup(SentinelError::TransactionScannerChannelClosed);
            };

            // Handle the message
            if let Err(error) = self.process_message(message).await {
                warn!(reason = %error, "emitting cancel signal");
                return cleanup(error);
            }
        }
    }

    #[tracing::instrument(skip_all, err)]
    async fn process_message(
        &self,
        message: TransactionScannerMessage,
    ) -> Result<(), SentinelError> {
        // Resolve the TransactionScanner message, expecting to obtain a join handle
        // that resolves into a `SolanaTransaction`.
        let join_handle = match message {
            Message(join_handle) => join_handle,
            Terminated(error) => {
                warn!(%error, "TransactionScanner terminated");
                Err(error)?
            }
        };

        // Wait for either the join handle to resolve or the cancellation signal.
        let rpc_result = tokio::select! {
            _ = self.cancellation_token.cancelled() => return Err(SentinelError::Stopped),
            res = join_handle => res?
        };

        // Resolve the outcome of the 'fetch transaction' RPC call
        match rpc_result {
            // Don't halt operation for non-fatal errors
            Err(TransactionRetrieverError::NonFatal(non_fatal_error)) => {
                warn!(error = %non_fatal_error, r#type = "non-fatal");
                Ok(())
            }

            // Fatal errors should interrupt the operation.
            Err(fatal) => Err(fatal)?,

            // Continue processing the Solana transaction
            Ok(solana_transaction) => self.process_transaction(solana_transaction).await,
        }
    }

    /// Searches for Gateway logs within a `SolanaTransaction` and process each
    /// one, in order.
    #[tracing::instrument(level = "trace", skip_all, fields(solana_transaction = %solana_transaction.signature), err)]
    async fn process_transaction(
        &self,
        solana_transaction: SolanaTransaction,
    ) -> Result<(), SentinelError> {
        let gateway_events = solana_transaction
            .logs
            .iter()
            .enumerate() // Enumerate before filtering to keep indices consistent
            .filter_map(|(tx_index, log)| {
                GatewayEvent::parse_log(log).map(|event| (tx_index, event))
            });

        for (tx_index, event) in gateway_events {
            match event {
                GatewayEvent::CallContract(data) => {
                    let CallContract {
                        sender,
                        destination_chain,
                        destination_address,
                        payload,
                        ..
                    } = data.into_owned();
                    self.handle_gateway_call_contract_event(
                        solana_transaction.signature,
                        tx_index,
                        sender,
                        destination_chain,
                        destination_address,
                        payload,
                    )
                    .await?
                }

                GatewayEvent::OperatorshipTransferred(_data) => {
                    todo!("Handle Operatorship Transferred event")
                }
                _ => unimplemented!(),
            };
        }
        Ok(())
    }

    /// Tries to build an `AxelarMessage` and send it to the Axelar Verifier
    /// component.
    #[tracing::instrument(
        level = "debug",
        skip_all,
        fields(transaction_signature, transaction_index),
        err
    )]
    async fn handle_gateway_call_contract_event(
        &self,
        transaction_signature: Signature,
        transaction_index: usize,
        sender: Pubkey,
        destination_chain: Vec<u8>,
        destination_address: Vec<u8>,
        payload: Vec<u8>,
    ) -> Result<(), SentinelError> {
        let message_ccid = format!(
            "{}:{}:{}",
            SOLANA_CHAIN_NAME, transaction_signature, transaction_index,
        );
        let message = amplifier_api::Message {
            id: message_ccid,
            source_chain: SOLANA_CHAIN_NAME.into(),
            source_address: hex::encode(sender.to_bytes()),
            destination_chain: String::from_utf8(destination_chain)
                .map_err(SentinelError::ByteVecParsing)?,
            destination_address: hex::encode(destination_address),
            payload,
        };

        info!(?message, "delivering message to Axelar Verifier");

        let message = SolanaToAxelarMessage {
            message,
            signature: transaction_signature,
        };

        self.verifier_channel
            .send(message)
            .await
            .map_err(|message| SentinelError::SendMessageError(message.0.message.id))
    }
}
