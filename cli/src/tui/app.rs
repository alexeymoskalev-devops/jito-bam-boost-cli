use std::collections::HashSet;

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use solana_pubkey::Pubkey;

use crate::batch_claim::{ClaimEvent, ClaimState};
use crate::scanner::EpochStatus;

pub const DEFAULT_MAINNET_RPC: &str = "https://api.mainnet-beta.solana.com";
pub const DEFAULT_TESTNET_RPC: &str = "https://api.testnet.solana.com";

/// Which screen of the TUI is currently active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Setup,
    Dashboard,
    Confirm,
    Progress,
}

/// Tab order of the fields on the Setup screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupField {
    Network,
    RpcUrl,
    Claimant,
    Start,
}

impl SetupField {
    fn next(self) -> Self {
        match self {
            SetupField::Network => SetupField::RpcUrl,
            SetupField::RpcUrl => SetupField::Claimant,
            SetupField::Claimant => SetupField::Start,
            SetupField::Start => SetupField::Network,
        }
    }

    fn prev(self) -> Self {
        match self {
            SetupField::Network => SetupField::Start,
            SetupField::RpcUrl => SetupField::Network,
            SetupField::Claimant => SetupField::RpcUrl,
            SetupField::Start => SetupField::Claimant,
        }
    }
}

/// Raw inputs the TUI event loop feeds into the reducer.
#[derive(Debug)]
pub enum AppEvent {
    Key(KeyEvent),
    ScanFinished(Result<Vec<EpochStatus>, String>),
    Claim(ClaimEvent),
    ClaimRunFinished,
    ClaimRunFailed(String),
}

/// Side effects the reducer asks the caller (event loop) to perform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    StartScan,
    StartClaim {
        epochs: Vec<u64>,
        keypair_path: String,
    },
    Quit,
}

/// The full TUI state; `handle` is the pure reducer over `AppEvent`.
pub struct App {
    pub screen: Screen,
    pub network: String,
    pub rpc_url: String,
    pub claimant_input: String,
    pub keypair_input: String,
    pub setup_focus: SetupField,
    pub setup_error: Option<String>,
    pub statuses: Vec<EpochStatus>,
    pub cursor: usize,
    pub selected: HashSet<u64>,
    pub scanning: bool,
    pub progress_rows: Vec<ClaimEvent>,
    pub claim_done: bool,
    pub progress_error: Option<String>,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self {
            screen: Screen::Setup,
            network: "mainnet".to_string(),
            rpc_url: DEFAULT_MAINNET_RPC.to_string(),
            claimant_input: String::new(),
            keypair_input: String::new(),
            setup_focus: SetupField::Network,
            setup_error: None,
            statuses: Vec::new(),
            cursor: 0,
            selected: HashSet::new(),
            scanning: false,
            progress_rows: Vec::new(),
            claim_done: false,
            progress_error: None,
        }
    }

    /// Pure reducer: applies one event to the state, returning an optional
    /// side effect for the caller to perform.
    pub fn handle(&mut self, event: AppEvent) -> Option<Action> {
        match event {
            AppEvent::Key(key) => self.handle_key(key),
            AppEvent::ScanFinished(result) => {
                self.handle_scan_finished(result);
                None
            }
            AppEvent::Claim(ev) => {
                self.handle_claim_event(ev);
                None
            }
            AppEvent::ClaimRunFinished => {
                self.claim_done = true;
                None
            }
            AppEvent::ClaimRunFailed(msg) => {
                self.claim_done = true;
                self.progress_error = Some(msg);
                None
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        match self.screen {
            Screen::Setup => self.handle_setup_key(key),
            Screen::Dashboard => self.handle_dashboard_key(key),
            Screen::Confirm => self.handle_confirm_key(key),
            Screen::Progress => self.handle_progress_key(key),
        }
    }

    fn handle_setup_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Tab | KeyCode::Down => {
                self.setup_focus = self.setup_focus.next();
                None
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.setup_focus = self.setup_focus.prev();
                None
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
                if self.setup_focus == SetupField::Network =>
            {
                self.toggle_network();
                None
            }
            KeyCode::Char(c)
                if matches!(self.setup_focus, SetupField::RpcUrl | SetupField::Claimant) =>
            {
                self.field_for_focus_mut().push(c);
                None
            }
            KeyCode::Backspace
                if matches!(self.setup_focus, SetupField::RpcUrl | SetupField::Claimant) =>
            {
                self.field_for_focus_mut().pop();
                None
            }
            KeyCode::Enter if self.setup_focus == SetupField::Start => self.try_start_scan(),
            KeyCode::Esc => Some(Action::Quit),
            _ => None,
        }
    }

    fn toggle_network(&mut self) {
        let new_network = if self.network == "mainnet" {
            "testnet"
        } else {
            "mainnet"
        };
        if self.rpc_url == DEFAULT_MAINNET_RPC || self.rpc_url == DEFAULT_TESTNET_RPC {
            self.rpc_url = if new_network == "mainnet" {
                DEFAULT_MAINNET_RPC.to_string()
            } else {
                DEFAULT_TESTNET_RPC.to_string()
            };
        }
        self.network = new_network.to_string();
    }

    fn field_for_focus_mut(&mut self) -> &mut String {
        match self.setup_focus {
            SetupField::RpcUrl => &mut self.rpc_url,
            SetupField::Claimant => &mut self.claimant_input,
            _ => unreachable!("field_for_focus_mut called for a non-text field"),
        }
    }

    fn try_start_scan(&mut self) -> Option<Action> {
        match self.claimant_input.parse::<Pubkey>() {
            Ok(_) => {
                self.setup_error = None;
                self.scanning = true;
                self.screen = Screen::Dashboard;
                Some(Action::StartScan)
            }
            Err(_) => {
                self.setup_error = Some(format!("invalid pubkey: {}", self.claimant_input));
                None
            }
        }
    }

    fn handle_scan_finished(&mut self, result: Result<Vec<EpochStatus>, String>) {
        self.scanning = false;
        match result {
            Ok(statuses) => {
                self.statuses = statuses;
                self.cursor = 0;
                self.selected.clear();
            }
            Err(msg) => {
                self.setup_error = Some(msg);
                self.screen = Screen::Setup;
            }
        }
    }

    fn handle_dashboard_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Up => {
                self.cursor = self.cursor.saturating_sub(1);
                None
            }
            KeyCode::Down => {
                let max = self.statuses.len().saturating_sub(1);
                self.cursor = (self.cursor + 1).min(max);
                None
            }
            KeyCode::Char(' ') => {
                self.toggle_cursor_selection();
                None
            }
            KeyCode::Char('a') => {
                self.selected = self.claimable_epochs();
                None
            }
            KeyCode::Char('c') => {
                if !self.selected.is_empty() {
                    self.screen = Screen::Confirm;
                }
                None
            }
            KeyCode::Char('r') => {
                self.scanning = true;
                Some(Action::StartScan)
            }
            KeyCode::Char('q') | KeyCode::Esc => Some(Action::Quit),
            _ => None,
        }
    }

    fn toggle_cursor_selection(&mut self) {
        let Some(status) = self.statuses.get(self.cursor) else {
            return;
        };
        if !status.is_claimable() {
            return;
        }
        let epoch = status.epoch;
        if !self.selected.remove(&epoch) {
            self.selected.insert(epoch);
        }
    }

    fn claimable_epochs(&self) -> HashSet<u64> {
        self.statuses
            .iter()
            .filter(|s| s.is_claimable())
            .map(|s| s.epoch)
            .collect()
    }

    fn handle_confirm_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Char('y') => {
                if self.keypair_input.is_empty() {
                    self.setup_error = Some("keypair path is required".to_string());
                    None
                } else {
                    self.confirm_claim()
                }
            }
            KeyCode::Char('n') | KeyCode::Esc => {
                self.screen = Screen::Dashboard;
                None
            }
            KeyCode::Char(c) => {
                self.keypair_input.push(c);
                None
            }
            KeyCode::Backspace => {
                self.keypair_input.pop();
                None
            }
            _ => None,
        }
    }

    fn confirm_claim(&mut self) -> Option<Action> {
        self.screen = Screen::Progress;
        self.progress_rows.clear();
        self.progress_error = None;
        self.claim_done = false;
        let mut epochs: Vec<u64> = self.selected.iter().copied().collect();
        epochs.sort_unstable();
        Some(Action::StartClaim {
            epochs,
            keypair_path: self.keypair_input.clone(),
        })
    }

    fn handle_claim_event(&mut self, ev: ClaimEvent) {
        if let Some(last) = self.progress_rows.last_mut() {
            if last.epoch == ev.epoch && last.state == ClaimState::Started {
                *last = ev;
                return;
            }
        }
        self.progress_rows.push(ev);
    }

    fn handle_progress_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc if self.claim_done => Some(Action::Quit),
            KeyCode::Char('b') if self.claim_done => {
                self.screen = Screen::Dashboard;
                self.scanning = true;
                Some(Action::StartScan)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> AppEvent {
        AppEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn scanned_app() -> App {
        let mut app = App::new();
        app.claimant_input = "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn".into();
        app.setup_focus = SetupField::Start;
        assert!(matches!(
            app.handle(key(KeyCode::Enter)),
            Some(Action::StartScan)
        ));
        app.handle(AppEvent::ScanFinished(Ok(vec![
            EpochStatus {
                epoch: 1,
                amount: Some(10),
                claimed: false,
            },
            EpochStatus {
                epoch: 2,
                amount: Some(20),
                claimed: true,
            },
            EpochStatus {
                epoch: 3,
                amount: None,
                claimed: false,
            },
        ])));
        app
    }

    #[test]
    fn setup_enter_on_start_triggers_scan_and_moves_to_dashboard() {
        let app = scanned_app();
        assert!(matches!(app.screen, Screen::Dashboard));
        assert_eq!(app.statuses.len(), 3);
        assert!(!app.scanning);
    }

    #[test]
    fn setup_rejects_invalid_claimant() {
        let mut app = App::new();
        app.claimant_input = "not-a-pubkey".into();
        app.setup_focus = SetupField::Start;
        assert!(app.handle(key(KeyCode::Enter)).is_none());
        assert!(app.setup_error.is_some());
        assert!(matches!(app.screen, Screen::Setup));
    }

    #[test]
    fn select_all_unclaimed_picks_only_eligible_unclaimed() {
        let mut app = scanned_app();
        app.handle(key(KeyCode::Char('a')));
        assert_eq!(app.selected, std::collections::HashSet::from([1]));
    }

    #[test]
    fn claim_flow_confirm_then_progress() {
        let mut app = scanned_app();
        app.handle(key(KeyCode::Char('a')));
        app.handle(key(KeyCode::Char('c')));
        assert!(matches!(app.screen, Screen::Confirm));
        app.keypair_input = "/tmp/id.json".into();
        let action = app.handle(key(KeyCode::Char('y')));
        match action {
            Some(Action::StartClaim {
                epochs,
                keypair_path,
            }) => {
                assert_eq!(epochs, vec![1]);
                assert_eq!(keypair_path, "/tmp/id.json");
            }
            other => panic!("expected StartClaim, got {other:?}"),
        }
        assert!(matches!(app.screen, Screen::Progress));
        app.handle(AppEvent::Claim(ClaimEvent {
            epoch: 1,
            state: ClaimState::Started,
        }));
        app.handle(AppEvent::Claim(ClaimEvent {
            epoch: 1,
            state: ClaimState::Success("sig".into()),
        }));
        app.handle(AppEvent::ClaimRunFinished);
        assert!(app.claim_done);
        assert_eq!(
            app.progress_rows.last().unwrap().state,
            ClaimState::Success("sig".into())
        );
    }

    #[test]
    fn q_quits_from_dashboard() {
        let mut app = scanned_app();
        assert!(matches!(
            app.handle(key(KeyCode::Char('q'))),
            Some(Action::Quit)
        ));
    }

    #[test]
    fn scan_finished_err_routes_back_to_setup_with_error() {
        let mut app = App::new();
        app.claimant_input = "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn".into();
        app.setup_focus = SetupField::Start;
        assert!(matches!(
            app.handle(key(KeyCode::Enter)),
            Some(Action::StartScan)
        ));
        assert!(app
            .handle(AppEvent::ScanFinished(Err("boom".into())))
            .is_none());
        assert!(!app.scanning);
        assert!(matches!(app.screen, Screen::Setup));
        assert_eq!(app.setup_error.as_deref(), Some("boom"));
    }

    #[test]
    fn claim_run_failed_marks_done_sets_error_and_q_quits() {
        let mut app = scanned_app();
        app.handle(key(KeyCode::Char('a')));
        app.handle(key(KeyCode::Char('c')));
        app.keypair_input = "/tmp/id.json".into();
        app.handle(key(KeyCode::Char('y')));
        assert!(matches!(app.screen, Screen::Progress));

        assert!(app
            .handle(AppEvent::ClaimRunFailed("keypair read failed".into()))
            .is_none());
        assert!(app.claim_done);
        assert_eq!(app.progress_error.as_deref(), Some("keypair read failed"));

        assert!(matches!(
            app.handle(key(KeyCode::Char('q'))),
            Some(Action::Quit)
        ));
    }
}
