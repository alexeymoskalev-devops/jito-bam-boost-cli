pub mod app;
pub mod ui;

use std::sync::Arc;

use ratatui::crossterm::event::{self, Event};
use solana_commitment_config::CommitmentConfig;
use solana_keypair::read_keypair_file;
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;
use tokio::sync::mpsc;

use crate::bam_boost_handler::default_cache_dir;
use crate::batch_claim::claim_epochs;
use crate::cli_config::CliConfig;
use crate::scanner::Scanner;
use app::{Action, App, AppEvent};

pub async fn run(
    network: Option<String>,
    rpc_url: String,
    commitment: CommitmentConfig,
    signer_path: Option<String>,
    program_id: Pubkey,
) -> anyhow::Result<()> {
    let mut app = App::new();
    if let Some(network) = network {
        app.network = network;
    }
    app.rpc_url = rpc_url;
    if let Some(path) = &signer_path {
        // Pre-fill: signer keypair gives us both claimant and claim signing.
        let keypair =
            read_keypair_file(path).map_err(|e| anyhow::anyhow!("Failed to read keypair: {e}"))?;
        app.claimant_input = solana_keypair::Signer::pubkey(&keypair).to_string();
        app.keypair_input = path.clone();
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();

    // Keyboard reader thread → AppEvent::Key
    let key_tx = tx.clone();
    std::thread::spawn(move || loop {
        match event::read() {
            Ok(Event::Key(key)) if key.kind == event::KeyEventKind::Press => {
                if key_tx.send(AppEvent::Key(key)).is_err() {
                    break;
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    });

    let mut terminal = ratatui::init();
    let result = event_loop(
        &mut terminal,
        &mut app,
        &tx,
        &mut rx,
        commitment,
        program_id,
    )
    .await;
    ratatui::restore();
    result
}

async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    tx: &mpsc::UnboundedSender<AppEvent>,
    rx: &mut mpsc::UnboundedReceiver<AppEvent>,
    commitment: CommitmentConfig,
    program_id: Pubkey,
) -> anyhow::Result<()> {
    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;
        let Some(event) = rx.recv().await else {
            return Ok(());
        };
        match app.handle(event) {
            Some(Action::Quit) => return Ok(()),
            Some(Action::StartScan) => {
                let tx = tx.clone();
                let network = app.network.clone();
                let rpc_url = app.rpc_url.clone();
                let claimant: Pubkey = match app.claimant_input.parse() {
                    Ok(claimant) => claimant,
                    Err(e) => {
                        let _ = tx.send(AppEvent::ScanFinished(Err(format!(
                            "invalid claimant pubkey: {e}"
                        ))));
                        continue;
                    }
                };
                tokio::spawn(async move {
                    let scanner = Scanner::new(default_cache_dir());
                    let rpc = RpcClient::new_with_commitment(rpc_url, commitment);
                    let result = scanner
                        .scan(&network, &claimant, &rpc, &program_id)
                        .await
                        .map_err(|e| e.to_string());
                    let _ = tx.send(AppEvent::ScanFinished(result));
                });
            }
            Some(Action::StartClaim {
                epochs,
                keypair_path,
            }) => {
                let tx = tx.clone();
                let network = app.network.clone();
                let rpc_url = app.rpc_url.clone();
                tokio::spawn(async move {
                    let outcome = run_claims(
                        &network,
                        rpc_url,
                        commitment,
                        &keypair_path,
                        program_id,
                        &epochs,
                        &tx,
                    )
                    .await;
                    match outcome {
                        Ok(()) => {
                            let _ = tx.send(AppEvent::ClaimRunFinished);
                        }
                        Err(e) => {
                            let _ = tx.send(AppEvent::ClaimRunFailed(e.to_string()));
                        }
                    }
                });
            }
            None => {}
        }
    }
}

async fn run_claims(
    network: &str,
    rpc_url: String,
    commitment: CommitmentConfig,
    keypair_path: &str,
    program_id: Pubkey,
    epochs: &[u64],
    tx: &mpsc::UnboundedSender<AppEvent>,
) -> anyhow::Result<()> {
    let keypair = read_keypair_file(keypair_path)
        .map_err(|e| anyhow::anyhow!("Failed to read keypair: {e}"))?;
    let cli_config = CliConfig {
        rpc_url,
        commitment,
        signer: Some(Arc::new(keypair)),
    };
    let scanner = Scanner::new(default_cache_dir());
    let progress_tx = tx.clone();
    claim_epochs(
        &scanner,
        &cli_config,
        &program_id,
        network,
        epochs,
        &mut move |event| {
            let _ = progress_tx.send(AppEvent::Claim(event));
        },
    )
    .await?;
    Ok(())
}
