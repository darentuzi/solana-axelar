use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use cmd::solana::SolanaContract;
use ethers::core::k256::ecdsa::SigningKey;
use ethers::middleware::SignerMiddleware;
use ethers::signers::LocalWallet;
use eyre::Context;
use url::Url;

pub(crate) mod cmd;

/// Xtask is the Axelar Solana workspace CLI that helps
/// both actors, humans and CI to achieve mundane tasks
/// like building, deploying and initializing Solana
/// programs.
#[derive(Parser)]
#[command(version, about, long_about = None)]
pub(crate) enum Cli {
    /// Build, deploy, instantiate and interact with our Solana programs
    Solana {
        #[command(subcommand)]
        command: Solana,
    },
    /// Delpoy, instantiate and operate with evm chains and our demo contracts
    Evm {
        /// The URL of the node to connect to
        #[arg(short, long)]
        node_rpc: Url,
        /// The private key of the account that will send the tx
        #[arg(short, long)]
        admin_private_key: LocalWallet,
        /// The command to execute
        #[command(subcommand)]
        command: Evm,
    },
    /// Work with cosmwasm contracts and the axelar chain
    Cosmwasm {
        #[command(subcommand)]
        command: Cosmwasm,
    },
}

#[derive(Subcommand)]
pub(crate) enum Cosmwasm {
    /// Build all cosmwasm contracts so that they would be ready for deployment
    Build,
    /// Deploy
    Deploy {
        #[arg(short, long)]
        private_key_hex: String,
    },
    Init {
        #[arg(short, long)]
        code_id: u64,
        #[arg(short, long)]
        private_key_hex: String,
        #[command(subcommand)]
        command: CosmwasmInit,
    },
    /// Generate a new Axelar wallet, outputs the Axelar bech32 key and the hex
    /// private key
    GenerateWallet,
}

/// Initialize contracts by providing their specific init parameters.
#[derive(Subcommand)]
pub(crate) enum CosmwasmInit {
    /// Initialize an already deployed voting verifier contract.
    VotingVerifier {
        #[arg(long)]
        chain_name: String,
    },
    /// Initialize an already deployed gateway contract
    Gateway {
        #[arg(short, long)]
        voting_verifier_address: String,
    },
    // Initialize an already deployed multisig prover contract.
    MultisigProver {
        #[arg(long)]
        chain_id: u64,
        #[arg(long)]
        gateway_address: String,
        #[arg(long)]
        voting_verifier_address: String,
        #[arg(long)]
        chain_name: String,
    },
}
/// The contracts are pre-built as ensured by the `evm-contracts-rs` crate in
/// our workspace. On EVM we don't differentiate deployment fron initialization
/// as we do on Solana.
#[derive(Subcommand)]
pub(crate) enum Evm {
    DeployAxelarMemo {
        #[arg(short, long)]
        gateway_contract_address: ethers::types::Address,
    },
    SendMemoToSolana {
        #[arg(short, long)]
        evm_memo_contract_address: ethers::types::Address,
        #[arg(short, long)]
        memo_to_send: String,
        #[arg(short, long)]
        solana_chain_id: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum Solana {
    /// Build's a contract that is listed in the programs
    /// workspace directory.
    Build {
        /// It accepts the name of the contract folder as argument.
        #[arg(value_enum)]
        contract: SolanaContract,
    },
    /// Deploys the given contract name
    Deploy {
        /// It accepts the name of the contract folder as argument.
        #[arg(value_enum)]
        contract: SolanaContract,
        /// They keypair used to deploy the contract and sign transactions.
        /// If not provided, it will fallback into Solana CLI defaults.
        #[arg(short, long)]
        keypair_path: Option<PathBuf>,
        /// The RPC URL of the target validator. If not provided, it will
        /// fallback into Solana CLI defaults.
        #[arg(short, long)]
        url: Option<Url>,
        /// The websocket URL of the target validator. Normally the same as the
        /// rpc url, but replacing scheme in favour of ws:// . If not
        /// provided, it will fallback into Solana CLI defaults.
        #[arg(short, long)]
        ws_url: Option<Url>,
        /// The file path to the solana program that's associated with the
        /// hardcoded program id
        #[arg(short, long)]
        program_id: PathBuf,
        // ---
        // TODO: expose "upgrate_authority"
    },
    Init {
        #[command(subcommand)]
        contract: SolanaInitSubcommand,
    },
}

/// Initialize contracts by providing their specific init parameters.
#[derive(Subcommand)]
pub(crate) enum SolanaInitSubcommand {
    /// Initialize an already deployed gateway contract.
    GmpGateway {
        /// A path that points to a toml file that contains the signers and
        /// their respective weights data. See `tests/auth_weighted.toml` file
        /// for an example.
        #[arg(short, long)]
        auth_weighted_file: PathBuf,
        /// The RPC URL of the target validator.
        /// If not provided, this will fallback in solana CLI current
        /// configuration.
        #[arg(short, long)]
        rpc_url: Option<Url>,
        /// The payer keypair file. This is a file containing the byte slice
        /// serialization of a `solana_sdk::signer::keypair::Keypair` .
        /// If not provided, this will fallback in solana CLI current
        /// configuration.
        #[arg(short, long)]
        payer_kp_path: Option<PathBuf>,
    },
}

impl Cli {
    pub(crate) async fn run(self) -> eyre::Result<()> {
        match self {
            Cli::Solana { command } => handle_solana(command).await?,
            Cli::Evm {
                node_rpc,
                admin_private_key,
                command,
            } => handle_evm(node_rpc, admin_private_key, command).await?,
            Cli::Cosmwasm { command } => handle_cosmwasm(command).await?,
        };
        Ok(())
    }
}

async fn handle_solana(command: Solana) -> Result<(), eyre::Error> {
    match command {
        Solana::Build { contract } => {
            cmd::solana::build_contract(contract)?;
        }
        Solana::Deploy {
            contract,
            keypair_path,
            url,
            ws_url,
            program_id,
        } => {
            cmd::solana::deploy(contract, program_id.as_path(), &keypair_path, &url, &ws_url)?;
        }
        Solana::Init { contract } => match &contract {
            SolanaInitSubcommand::GmpGateway {
                auth_weighted_file,
                rpc_url,
                payer_kp_path,
            } => {
                cmd::solana::init_gmp_gateway(auth_weighted_file, rpc_url, payer_kp_path).await?;
            }
        },
    };
    Ok(())
}

async fn handle_evm(
    node_rpc: Url,
    admin_private_key: LocalWallet,
    command: Evm,
) -> Result<(), eyre::Error> {
    let signer = init_evm_signer(&node_rpc, admin_private_key.clone()).await;
    let signer = evm_contracts_test_suite::EvmSigner {
        wallet: admin_private_key.clone(),
        signer,
    };
    match command {
        Evm::DeployAxelarMemo {
            gateway_contract_address,
        } => {
            cmd::evm::deploy_axelar_memo(signer, gateway_contract_address).await?;
        }
        Evm::SendMemoToSolana {
            evm_memo_contract_address,
            memo_to_send,
            solana_chain_id,
        } => {
            cmd::evm::send_memo_to_solana(
                signer,
                evm_memo_contract_address,
                memo_to_send,
                solana_chain_id,
            )
            .await?;
        }
    };
    Ok(())
}
async fn handle_cosmwasm(command: Cosmwasm) -> Result<(), eyre::Error> {
    match command {
        Cosmwasm::Build => {
            cmd::cosmwasm::build().await?;
        }
        Cosmwasm::Deploy { private_key_hex } => {
            let key_bytes = hex::decode(private_key_hex)?;
            let signing_key = cosmrs::crypto::secp256k1::SigningKey::from_slice(&key_bytes)
                .context("invalid secp256k1 private key")?;
            cmd::cosmwasm::deploy(signing_key).await?;
        }
        Cosmwasm::GenerateWallet => cmd::cosmwasm::generate_wallet()?,
        Cosmwasm::Init {
            code_id,
            command,
            private_key_hex,
        } => {
            let key_bytes = hex::decode(private_key_hex)?;
            let signing_key = cosmrs::crypto::secp256k1::SigningKey::from_slice(&key_bytes)
                .context("invalid secp256k1 private key")?;
            match command {
                CosmwasmInit::VotingVerifier { chain_name } => {
                    cmd::cosmwasm::init_voting_verifier(code_id, signing_key, chain_name).await?;
                }
                CosmwasmInit::Gateway {
                    voting_verifier_address,
                } => {
                    cmd::cosmwasm::init_gateway(code_id, signing_key, voting_verifier_address)
                        .await?;
                }
                CosmwasmInit::MultisigProver {
                    chain_id,
                    gateway_address,
                    voting_verifier_address,
                    chain_name,
                } => {
                    cmd::cosmwasm::init_multisig_prover(
                        code_id,
                        signing_key,
                        chain_id,
                        gateway_address,
                        voting_verifier_address,
                        chain_name,
                    )
                    .await?;
                }
            }
        }
    };
    Ok(())
}

async fn init_evm_signer(
    node_rpc: &Url,
    wallet: LocalWallet,
) -> Arc<
    SignerMiddleware<
        Arc<ethers::providers::Provider<ethers::providers::Http>>,
        ethers::signers::Wallet<SigningKey>,
    >,
> {
    let provider =
        ethers::providers::Provider::<ethers::providers::Http>::try_from(node_rpc.as_str())
            .expect("URL is always valid")
            .interval(std::time::Duration::from_millis(200));
    let provider = Arc::new(provider);
    let client = SignerMiddleware::new_with_provider_chain(provider, wallet)
        .await
        .unwrap();

    Arc::new(client)
}
