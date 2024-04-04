use crate::{
    config, healthcheck, sentinel::SolanaSentinel, state::State, transports::SolanaToAxelarMessage,
    verifier::AxelarVerifier,
};
use anyhow::{Context, Result};
use futures_util::FutureExt;
use std::net::SocketAddr;
use tokio::{
    join, pin, select,
    sync::mpsc::{self, Receiver, Sender},
    task::JoinSet,
    time::{timeout, Duration},
};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

const TIMEOUT: Duration = Duration::from_secs(30);

pub struct Relayer {
    axelar_to_solana: Option<AxelarToSolanaHandler>,
    solana_to_axelar: Option<SolanaToAxelarHandler>,
    health_check_server_addr: SocketAddr,
}

impl Relayer {
    pub async fn from_config(config: config::Config) -> Result<Self> {
        config.validate()?;

        let config::Config {
            axelar_to_solana,
            solana_to_axelar,
            database,
            healthcheck_bind_addr,
        } = config;

        let state = State::from_url(database.url)
            .await
            .context("Failed to connect to Relayer database")?;

        state.migrate().await?;

        Ok(Relayer {
            axelar_to_solana: axelar_to_solana.map(|config| {
                AxelarToSolanaHandler::new(config.approver, config.includer, state.clone())
            }),
            solana_to_axelar: solana_to_axelar
                .map(|config| SolanaToAxelarHandler::new(config.sentinel, config.verifier, state)),
            health_check_server_addr: healthcheck_bind_addr,
        })
    }

    pub async fn run(self) {
        // Start the health check server.
        let _health_check_server = match healthcheck::start(self.health_check_server_addr).await {
            Ok(guard) => {
                info!(address = %self.health_check_server_addr, "Started health check server.");
                guard
            }
            Err(error) => {
                error!(%error, "Failed to start the health check server.");
                return;
            }
        };

        match (self.axelar_to_solana, self.solana_to_axelar) {
            (Some(axelar_to_solana), Some(solana_to_axelar)) => {
                let mut set = JoinSet::new();
                set.spawn(solana_to_axelar.run());
                set.spawn(axelar_to_solana.run());

                // Await on both transports indefinitely.
                // Unexpected termination of either transport indicates a non-recoverable error,
                // requiring the Relayer to shut down.
                set.join_next().await;
                error!("One of the transports has terminated unexpectedly");
                return;
            }

            (Some(axelar_to_solana), None) => {
                let mut set = JoinSet::new();
                set.spawn(axelar_to_solana.run());
                set.join_next().await;
            }
            (None, Some(solana_to_axelar)) => {
                let mut set = JoinSet::new();
                set.spawn(solana_to_axelar.run());
                set.join_next().await;
            }
            (None, None) => {
                error!("Relayer was set to run without configured transports.")
            }
        };
        info!("Relayer is shutting down");
    }
}

//
// Transport types
//

// TODO: Transports look very similar. We can make a single type that is generic over its actors.

struct AxelarToSolanaHandler {
    approver: config::AxelarApprover,
    includer: config::SolanaIncluder,
    #[allow(dead_code)]
    state: State,
}

impl AxelarToSolanaHandler {
    fn new(
        approver: config::AxelarApprover,
        includer: config::SolanaIncluder,
        state: State,
    ) -> Self {
        Self {
            approver,
            includer,
            state,
        }
    }

    async fn run(self) {
        loop {
            info!("Starting Axelar to Solana transport");
            let (approver, includer) =
                AxelarToSolanaHandler::setup_actors(&self.approver, &self.includer);

            let mut set = JoinSet::new();
            set.spawn(approver.run());
            set.spawn(includer.run());

            while (set.join_next().await).is_some() {
                error!("Axelar to Solana transport failed. Restarting.");
                set.abort_all();
            }
        }
    }

    fn setup_actors(
        _approver_config: &config::AxelarApprover,
        _includer_config: &config::SolanaIncluder,
    ) -> (ApproverActor, IncluderActor) {
        // TODO: use config to properly initialize actors
        let (sender, receiver) = mpsc::channel(500); // FIXME: magic number
        let approver = ApproverActor { sender };
        let includer = IncluderActor { receiver };
        (approver, includer)
    }
}

struct SolanaToAxelarHandler {
    sentinel: config::SolanaSentinel,
    verifier: config::AxelarVerifier,
    state: State,
}

impl SolanaToAxelarHandler {
    fn new(
        sentinel: config::SolanaSentinel,
        verifier: config::AxelarVerifier,
        state: State,
    ) -> Self {
        Self {
            sentinel,
            verifier,
            state,
        }
    }

    /// Runs the Solana-to-Axelar transport indefinitely.
    ///
    /// This function sets up the necessary actors (Sentinel and Verifier) and runs them concurrently.
    /// If either fails for any reason, the entire transport will be restarted.
    #[tracing::instrument(skip(self), name = "solana-to-axelar-transport")]
    async fn run(self) {
        loop {
            info!("Starting Solana to Axelar transport");
            let (sentinel, verifier, cancellation_token) = SolanaToAxelarHandler::setup_actors(
                &self.sentinel,
                &self.verifier,
                self.state.clone(),
            );

            // Fuse the futures to allow polling of the other future when one is waiting for shutdown.
            pin! {
                let sentinel_future = sentinel.run().fuse();
                let verifier_future = verifier.run().fuse();
            }

            // Run the Sentinel and Verifier concurrently, and wait for the first one to fail.
            select! {
                _ = &mut sentinel_future => error!("Solana Sentinel has failed"),
                _ = &mut verifier_future => error!("Axelar Verifier has failed")
            }

            // Trigger cancellation and wait for both actors to gracefully shut down, up to `TIMEOUT`.
            cancellation_token.cancel();
            tracing::debug!(
                timeout = TIMEOUT.as_secs(),
                "Waiting for components to gracefully shutdown"
            );
            let _ = join!(
                timeout(TIMEOUT, sentinel_future),
                timeout(TIMEOUT, verifier_future)
            );
            tracing::warn!("Restarting Solana-to-Axelar transport")
        }
    }

    fn setup_actors(
        sentinel_config: &config::SolanaSentinel,
        verifier_config: &config::AxelarVerifier,
        state: State,
    ) -> (SolanaSentinel, AxelarVerifier, CancellationToken) {
        let (sender, receiver) = mpsc::channel::<SolanaToAxelarMessage>(500); // FIXME: magic number

        // This is the root cancelation token for this transport session.
        // It is also used to derive child tokens for each subcomponent, so they can manage their own cancelation schedules.
        // The root token will be cancelled if any subcomponent returns early.
        let transport_cancelation_token = CancellationToken::new();
        let sentinel_cancelation_token = transport_cancelation_token.child_token();
        let verifier_cancelation_token = transport_cancelation_token.child_token();

        // Solana Sentinel
        let config::SolanaSentinel {
            gateway_address,
            rpc,
        } = sentinel_config;
        let sentinel = SolanaSentinel::new(
            *gateway_address,
            rpc.clone(),
            sender,
            state.clone(),
            sentinel_cancelation_token,
        );

        // Axelar Verifier
        let verifier = AxelarVerifier::new(
            verifier_config.rpc.clone(),
            receiver,
            state,
            verifier_cancelation_token,
        );

        (sentinel, verifier, transport_cancelation_token)
    }
}

//
// Actor Placheholder Types
//

// TODO: Use the real worker types already defined in this crate.

struct ApproverActor {
    #[allow(dead_code)]
    sender: Sender<()>,
}

impl ApproverActor {
    async fn run(self) {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

#[allow(dead_code)]
struct IncluderActor {
    receiver: Receiver<()>,
}

impl IncluderActor {
    async fn run(self) {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }
}
