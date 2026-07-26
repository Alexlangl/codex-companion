use codex_companion_core::{
    default_codex_dir, CompanionStatus, GroupPolicy, ProviderConfig, ProviderGroup, ProviderKind,
    ProviderLaunchMode, RepairOptions, TokenUsageSummary,
};
use codex_companion_daemon::{provider_can_direct_connect, CompanionDaemon};
use codex_companion_provider::GroupUpsert;
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Terminal,
};
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Status,
    Providers,
    Groups,
    Repair,
    Token,
}

impl Tab {
    fn title(self) -> &'static str {
        match self {
            Tab::Status => "总览",
            Tab::Providers => "账号",
            Tab::Groups => "分组",
            Tab::Repair => "修复",
            Tab::Token => "用量",
        }
    }
}

#[derive(Debug)]
struct TuiState {
    tab: Tab,
    selected_provider: usize,
    selected_group: usize,
    message: String,
    token_usage: Option<TokenUsageSummary>,
}

impl Default for TuiState {
    fn default() -> Self {
        Self {
            tab: Tab::Status,
            selected_provider: 0,
            selected_group: 0,
            message: "按 1-5 切换页面，? 查看快捷键。".to_string(),
            token_usage: None,
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let daemon = CompanionDaemon::default()?;
    daemon.start_background_tasks();
    let mut app = TuiState::default();

    loop {
        let status = daemon.status()?;
        clamp_selection(&mut app, &status);
        terminal.draw(|frame| {
            let layout = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(22), Constraint::Min(60)])
                .split(frame.area());
            render_sidebar(frame, layout[0], app.tab);
            render_main(frame, layout[1], &app, &status);
        })?;

        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if key.code == KeyCode::Char('q') {
                    break;
                }
                handle_key(&daemon, &mut app, &status, key.code).await;
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

async fn handle_key(
    daemon: &CompanionDaemon,
    app: &mut TuiState,
    status: &CompanionStatus,
    key: KeyCode,
) {
    match key {
        KeyCode::Char('1') => app.tab = Tab::Status,
        KeyCode::Char('2') => app.tab = Tab::Providers,
        KeyCode::Char('3') => app.tab = Tab::Groups,
        KeyCode::Char('4') => app.tab = Tab::Repair,
        KeyCode::Char('5') => app.tab = Tab::Token,
        KeyCode::Char('?') => {
            app.message =
                "快捷键：j/k 选择，i 导入 JSON，a 添加 API Key，r 刷新，R 刷新全部，d 删除，n 新建分组，e 编辑分组，u 启用分组，g 启动分组，p 启动账号，x dry-run，f 修复，t 扫描用量。".to_string();
        }
        KeyCode::Char('j') | KeyCode::Down => move_selection(app, status, 1),
        KeyCode::Char('k') | KeyCode::Up => move_selection(app, status, -1),
        KeyCode::Char('i') if app.tab == Tab::Providers => import_json(daemon, app),
        KeyCode::Char('a') if app.tab == Tab::Providers => add_api_key(daemon, app),
        KeyCode::Char('r') if app.tab == Tab::Providers => {
            refresh_selected_provider(daemon, app, status).await
        }
        KeyCode::Char('R') => refresh_all(daemon, app).await,
        KeyCode::Char('d') if app.tab == Tab::Providers => {
            delete_selected_provider(daemon, app, status)
        }
        KeyCode::Char('p') if app.tab == Tab::Providers => {
            launch_selected_provider(daemon, app, status)
        }
        KeyCode::Char('n') if app.tab == Tab::Groups => create_group(daemon, app),
        KeyCode::Char('e') if app.tab == Tab::Groups => edit_group(daemon, app, status),
        KeyCode::Char('u') if app.tab == Tab::Groups => use_group(daemon, app, status),
        KeyCode::Char('g') if app.tab == Tab::Groups => launch_group(daemon, app, status),
        KeyCode::Char('x') if app.tab == Tab::Repair => repair(daemon, app, true),
        KeyCode::Char('f') if app.tab == Tab::Repair => repair(daemon, app, false),
        KeyCode::Char('t') if app.tab == Tab::Token => token_usage(daemon, app),
        _ => {}
    }
}

fn render_sidebar(frame: &mut ratatui::Frame<'_>, area: Rect, tab: Tab) {
    let items = [
        (Tab::Status, "1 总览"),
        (Tab::Providers, "2 账号"),
        (Tab::Groups, "3 分组"),
        (Tab::Repair, "4 修复"),
        (Tab::Token, "5 用量"),
    ]
    .into_iter()
    .map(|(item_tab, label)| {
        let style = if item_tab == tab {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        ListItem::new(label).style(style)
    })
    .collect::<Vec<_>>();
    frame.render_widget(
        List::new(items).block(Block::default().borders(Borders::ALL).title("CC")),
        area,
    );
}

fn render_main(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &TuiState,
    status: &CompanionStatus,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(format!("Codex Companion · {}", app.tab.title())),
            Line::from(format!(
                "Relay: {} · Codex: {}",
                status.relay_base_url, status.codex.message
            )),
        ])
        .block(Block::default().borders(Borders::ALL).title("状态")),
        chunks[0],
    );
    match app.tab {
        Tab::Status => render_status(frame, chunks[1], status),
        Tab::Providers => render_providers(frame, chunks[1], app, status),
        Tab::Groups => render_groups(frame, chunks[1], app, status),
        Tab::Repair => render_repair(frame, chunks[1], status),
        Tab::Token => render_token(frame, chunks[1], app),
    }
    frame.render_widget(
        Paragraph::new(app.message.as_str())
            .block(Block::default().borders(Borders::ALL).title("消息")),
        chunks[2],
    );
}

fn render_status(frame: &mut ratatui::Frame<'_>, area: Rect, status: &CompanionStatus) {
    let active = status
        .active_providers
        .iter()
        .map(|provider| provider.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(format!(
                "当前分组: {}",
                status
                    .active_group
                    .as_ref()
                    .map(|group| group.name.as_str())
                    .unwrap_or("未配置")
            )),
            Line::from(format!("账号数量: {}", status.config.providers.len())),
            Line::from(format!(
                "当前账号: {}",
                if active.is_empty() { "无" } else { &active }
            )),
            Line::from("R 刷新全部账号 · 2 管理账号 · 3 管理分组"),
        ])
        .block(Block::default().borders(Borders::ALL).title("运行状态")),
        area,
    );
}

fn render_providers(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &TuiState,
    status: &CompanionStatus,
) {
    let providers = providers(status);
    let items = providers
        .iter()
        .enumerate()
        .map(|(index, provider)| {
            let health = status.config.health.get(&provider.id);
            let prefix = if index == app.selected_provider {
                ">"
            } else {
                " "
            };
            ListItem::new(format!(
                "{} {} · {:?} · {} · {}",
                prefix,
                provider_label(provider),
                provider.kind,
                health
                    .map(|item| format!("{:?}", item.status))
                    .unwrap_or_else(|| "Unknown".to_string()),
                quota_label(provider)
            ))
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .title("账号 · i 导入 JSON · a 添加 API Key · r 刷新 · d 删除 · p 启动"),
        ),
        area,
    );
}

fn render_groups(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &TuiState,
    status: &CompanionStatus,
) {
    let groups = groups(status);
    let items = groups
        .iter()
        .enumerate()
        .map(|(index, group)| {
            let prefix = if index == app.selected_group {
                ">"
            } else {
                " "
            };
            let active = if group.id == status.config.relay.active_group_id {
                "当前"
            } else {
                ""
            };
            ListItem::new(format!(
                "{} {} · {} 个账号 · {:?} · {}",
                prefix,
                group.name,
                group.provider_order.len(),
                group.policy,
                active
            ))
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .title("分组 · n 新建 · e 编辑顺序 · u 设为当前 · g 启动 Codex"),
        ),
        area,
    );
}

fn render_repair(frame: &mut ratatui::Frame<'_>, area: Rect, status: &CompanionStatus) {
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(format!("Codex 目录: {}", status.codex.codex_dir.display())),
            Line::from("x dry-run 修复到 codex-companion"),
            Line::from("f 执行历史和插件 namespace 修复到 codex-companion"),
        ])
        .block(Block::default().borders(Borders::ALL).title("修复")),
        area,
    );
}

fn render_token(frame: &mut ratatui::Frame<'_>, area: Rect, app: &TuiState) {
    let lines = if let Some(stats) = app.token_usage.as_ref() {
        vec![
            Line::from(format!(
                "文件: {} · 会话: {} · 事件: {}",
                stats.files_scanned, stats.sessions, stats.events
            )),
            Line::from(format!(
                "总 Token: {} · 输入: {} · 缓存: {} · 输出: {}",
                stats.total_tokens,
                stats.input_tokens,
                stats.cached_input_tokens,
                stats.output_tokens
            )),
            Line::from("t 重新扫描"),
        ]
    } else {
        vec![Line::from("按 t 扫描 Codex token 统计。")]
    };
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("用量")),
        area,
    );
}

fn providers(status: &CompanionStatus) -> Vec<&ProviderConfig> {
    status.config.providers.values().collect()
}

fn groups(status: &CompanionStatus) -> Vec<&ProviderGroup> {
    status.config.groups.values().collect()
}

fn clamp_selection(app: &mut TuiState, status: &CompanionStatus) {
    app.selected_provider = app
        .selected_provider
        .min(status.config.providers.len().saturating_sub(1));
    app.selected_group = app
        .selected_group
        .min(status.config.groups.len().saturating_sub(1));
}

fn move_selection(app: &mut TuiState, status: &CompanionStatus, delta: isize) {
    let len = match app.tab {
        Tab::Providers => status.config.providers.len(),
        Tab::Groups => status.config.groups.len(),
        _ => 0,
    };
    if len == 0 {
        return;
    }
    let current = match app.tab {
        Tab::Providers => &mut app.selected_provider,
        Tab::Groups => &mut app.selected_group,
        _ => return,
    };
    let next = (*current as isize + delta).clamp(0, len.saturating_sub(1) as isize);
    *current = next as usize;
}

fn selected_provider<'a>(
    app: &TuiState,
    status: &'a CompanionStatus,
) -> Option<&'a ProviderConfig> {
    providers(status).get(app.selected_provider).copied()
}

fn selected_group<'a>(app: &TuiState, status: &'a CompanionStatus) -> Option<&'a ProviderGroup> {
    groups(status).get(app.selected_group).copied()
}

fn import_json(daemon: &CompanionDaemon, app: &mut TuiState) {
    let Some(path) = prompt("JSON 文件路径") else {
        return;
    };
    match fs::read_to_string(PathBuf::from(path.trim()))
        .map_err(|error| error.to_string())
        .and_then(|text| {
            daemon
                .import_provider_json_many(&text, None, None, None)
                .map_err(|error| error.to_string())
        }) {
        Ok(outcomes) => {
            app.message = format!(
                "已导入 {} 个账号，{} 个失败。",
                outcomes.succeeded.len(),
                outcomes.failed.len()
            )
        }
        Err(error) => app.message = format!("导入失败：{error}"),
    }
}

fn add_api_key(daemon: &CompanionDaemon, app: &mut TuiState) {
    let Some(name) = prompt("显示名称") else {
        return;
    };
    let Some(base_url) = prompt("Base URL，例如 https://api.example.com/v1") else {
        return;
    };
    let Some(api_key) = prompt("API Key") else {
        return;
    };
    match daemon.import_api_key_provider(
        name,
        ProviderKind::OpenAiCompatible,
        base_url,
        None,
        api_key,
        None,
        None,
        Some(60),
    ) {
        Ok(outcome) => app.message = format!("已添加账号：{}", outcome.provider.name),
        Err(error) => app.message = format!("添加失败：{error}"),
    }
}

async fn refresh_selected_provider(
    daemon: &CompanionDaemon,
    app: &mut TuiState,
    status: &CompanionStatus,
) {
    let Some(provider) = selected_provider(app, status) else {
        return;
    };
    match daemon.refresh_provider(&provider.id).await {
        Ok(health) => app.message = format!("已刷新 {}：{:?}", provider.name, health.status),
        Err(error) => app.message = format!("刷新失败：{error}"),
    }
}

async fn refresh_all(daemon: &CompanionDaemon, app: &mut TuiState) {
    match daemon.refresh_all_providers().await {
        Ok(items) => app.message = format!("已刷新 {} 个账号。", items.len()),
        Err(error) => app.message = format!("刷新失败：{error}"),
    }
}

fn delete_selected_provider(
    daemon: &CompanionDaemon,
    app: &mut TuiState,
    status: &CompanionStatus,
) {
    let Some(provider) = selected_provider(app, status) else {
        return;
    };
    match daemon.remove_provider(&provider.id) {
        Ok(true) => app.message = format!("已删除账号：{}", provider.name),
        Ok(false) => app.message = "账号不存在。".to_string(),
        Err(error) => app.message = format!("删除失败：{error}"),
    }
}

fn launch_selected_provider(
    daemon: &CompanionDaemon,
    app: &mut TuiState,
    status: &CompanionStatus,
) {
    let Some(provider) = selected_provider(app, status) else {
        return;
    };
    let mode = status
        .config
        .app
        .provider_launch_modes
        .get(&provider.id)
        .cloned()
        .unwrap_or_default();
    if provider_launch_will_direct(provider, &mode)
        && provider_direct_launch_writes_auth_json(
            provider,
            status.config.app.preserve_official_codex_auth,
        )
        && !confirm_direct_auth_write(provider)
    {
        app.message = format!("已取消启动账号：{}", provider.name);
        return;
    }
    match daemon.launch_provider_with_mode(&provider.id, None, mode) {
        Ok(outcome) => app.message = outcome.message,
        Err(error) => app.message = format!("启动失败：{error}"),
    }
}

fn provider_launch_will_direct(provider: &ProviderConfig, mode: &ProviderLaunchMode) -> bool {
    match mode {
        ProviderLaunchMode::Direct => true,
        ProviderLaunchMode::Relay => false,
        ProviderLaunchMode::Auto => provider_can_direct_connect(provider),
    }
}

fn provider_direct_launch_writes_auth_json(
    provider: &ProviderConfig,
    preserve_official_codex_auth: bool,
) -> bool {
    let has_file_auth = provider
        .direct_auth_ref
        .as_deref()
        .or(provider.auth_ref.as_deref())
        .map(str::trim)
        .is_some_and(|auth_ref| auth_ref.starts_with("file:"));
    has_file_auth
        && (matches!(provider.kind, ProviderKind::OfficialCodex) || !preserve_official_codex_auth)
}

fn confirm_direct_auth_write(provider: &ProviderConfig) -> bool {
    let answer = prompt(&format!(
        "{} 直连会合并写入 Codex auth.json。输入 YES 确认",
        provider_label(provider)
    ));
    matches!(answer.as_deref(), Some("YES"))
}

fn create_group(daemon: &CompanionDaemon, app: &mut TuiState) {
    let Some(name) = prompt("分组名称") else {
        return;
    };
    let Some(id) = prompt("分组 ID") else {
        return;
    };
    let Some(order) = prompt("账号 ID 顺序，逗号分隔") else {
        return;
    };
    save_group(daemon, app, id, name, parse_csv(&order), true);
}

fn edit_group(daemon: &CompanionDaemon, app: &mut TuiState, status: &CompanionStatus) {
    let Some(group) = selected_group(app, status) else {
        return;
    };
    let Some(order) = prompt("新的账号 ID 顺序，逗号分隔") else {
        return;
    };
    save_group(
        daemon,
        app,
        group.id.clone(),
        group.name.clone(),
        parse_csv(&order),
        group.fallback_enabled,
    );
}

fn save_group(
    daemon: &CompanionDaemon,
    app: &mut TuiState,
    id: String,
    name: String,
    provider_order: Vec<String>,
    fallback_enabled: bool,
) {
    match daemon.upsert_group(GroupUpsert {
        id,
        name,
        policy: if fallback_enabled {
            GroupPolicy::PriorityFallback
        } else {
            GroupPolicy::Manual
        },
        provider_order,
        provider_weights: Default::default(),
        fallback_enabled,
    }) {
        Ok(group) => app.message = format!("已保存分组：{}", group.name),
        Err(error) => app.message = format!("保存失败：{error}"),
    }
}

fn use_group(daemon: &CompanionDaemon, app: &mut TuiState, status: &CompanionStatus) {
    let Some(group) = selected_group(app, status) else {
        return;
    };
    match daemon.use_group(&group.id) {
        Ok(group) => app.message = format!("当前分组：{}", group.name),
        Err(error) => app.message = format!("启用失败：{error}"),
    }
}

fn launch_group(daemon: &CompanionDaemon, app: &mut TuiState, status: &CompanionStatus) {
    let Some(group) = selected_group(app, status) else {
        return;
    };
    match daemon.launch_group(&group.id, None) {
        Ok(outcome) => app.message = outcome.message,
        Err(error) => app.message = format!("启动失败：{error}"),
    }
}

fn repair(daemon: &CompanionDaemon, app: &mut TuiState, dry_run: bool) {
    let codex_dir = match default_codex_dir() {
        Ok(path) => path,
        Err(error) => {
            app.message = format!("读取 Codex 目录失败：{error}");
            return;
        }
    };
    match daemon.repair(RepairOptions {
        codex_dir,
        history: true,
        plugins: true,
        dry_run,
        target_provider_id: None,
    }) {
        Ok(outcome) => {
            app.message = format!(
                "{}：历史 {} 行，插件 {} 个。",
                if dry_run { "Dry-run" } else { "修复完成" },
                outcome.migrated_history_lines,
                outcome.migrated_plugin_files
            )
        }
        Err(error) => app.message = format!("修复失败：{error}"),
    }
}

fn token_usage(daemon: &CompanionDaemon, app: &mut TuiState) {
    let codex_dir = match default_codex_dir() {
        Ok(path) => path,
        Err(error) => {
            app.message = format!("读取 Codex 目录失败：{error}");
            return;
        }
    };
    match daemon.token_usage(codex_dir) {
        Ok(stats) => {
            app.message = "Token 统计已更新。".to_string();
            app.token_usage = Some(stats);
        }
        Err(error) => app.message = format!("Token 统计失败：{error}"),
    }
}

fn provider_label(provider: &ProviderConfig) -> String {
    let account_label = provider.account.as_ref().and_then(|account| {
        account
            .email
            .clone()
            .filter(|label| is_meaningful_account_label(label))
            .or_else(|| {
                account
                    .display_name
                    .clone()
                    .filter(|label| is_meaningful_account_label(label))
            })
            .or_else(|| {
                (!matches!(provider.kind, ProviderKind::OfficialCodex))
                    .then(|| account.account_id.clone())
                    .flatten()
            })
            .or_else(|| {
                (!matches!(provider.kind, ProviderKind::OfficialCodex))
                    .then(|| account.user_id.clone())
                    .flatten()
            })
    });
    account_label
        .or_else(|| is_meaningful_account_label(&provider.name).then(|| provider.name.clone()))
        .unwrap_or_else(|| {
            if matches!(provider.kind, ProviderKind::OfficialCodex) {
                "Codex 官方账号".to_string()
            } else {
                provider.id.clone()
            }
        })
}

fn is_meaningful_account_label(label: &str) -> bool {
    !matches!(
        label.trim().to_ascii_lowercase().as_str(),
        "codex 官方账号" | "官方账号" | "codex official account" | "official account"
    )
}

fn quota_label(provider: &ProviderConfig) -> String {
    provider
        .account
        .as_ref()
        .and_then(|account| {
            account
                .quota_percent
                .map(|value| format!("{value:.0}%"))
                .or_else(|| account.quota_label.clone())
        })
        .unwrap_or_else(|| "待刷新".to_string())
}

fn parse_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn prompt(label: &str) -> Option<String> {
    let _ = disable_raw_mode();
    print!("\n{label}: ");
    let _ = io::stdout().flush();
    let mut input = String::new();
    let result = io::stdin().read_line(&mut input).ok().map(|_| input);
    let _ = enable_raw_mode();
    result
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(auth_ref: Option<&str>) -> ProviderConfig {
        ProviderConfig {
            id: "provider".to_string(),
            name: "Provider".to_string(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: "https://example.com/v1".to_string(),
            websocket_url: None,
            auth_ref: auth_ref.map(ToOwned::to_owned),
            direct_auth_ref: None,
            model_map: Default::default(),
            priority: 100,
            enabled: true,
            refresh_interval_seconds: codex_companion_core::default_refresh_interval_seconds(),
            account: None,
        }
    }

    #[test]
    fn tui_launch_respects_relay_mode_when_direct_is_possible() {
        let provider = provider(Some("file:/tmp/provider-auth.json"));

        assert!(provider_launch_will_direct(
            &provider,
            &ProviderLaunchMode::Auto
        ));
        assert!(!provider_launch_will_direct(
            &provider,
            &ProviderLaunchMode::Relay
        ));
    }

    #[test]
    fn tui_requires_confirmation_for_file_auth_direct_writes() {
        assert!(provider_direct_launch_writes_auth_json(
            &provider(Some("file:/tmp/provider-auth.json")),
            false,
        ));
        assert!(!provider_direct_launch_writes_auth_json(
            &provider(Some("file:/tmp/provider-auth.json")),
            true,
        ));
        assert!(!provider_direct_launch_writes_auth_json(
            &provider(Some("env:OPENAI_API_KEY")),
            false,
        ));
        assert!(!provider_direct_launch_writes_auth_json(
            &provider(None),
            false,
        ));
        let mut official_provider = provider(Some("file:/tmp/official-auth.json"));
        official_provider.kind = ProviderKind::OfficialCodex;
        assert!(provider_direct_launch_writes_auth_json(
            &official_provider,
            true,
        ));
    }

    #[test]
    fn official_account_label_uses_friendly_fallback_instead_of_account_id() {
        let mut official_provider = provider(Some("file:/tmp/official-auth.json"));
        official_provider.name = "Codex 官方账号".to_string();
        official_provider.kind = ProviderKind::OfficialCodex;
        official_provider.account = Some(codex_companion_core::ProviderAccountInfo {
            email: Some("Codex 官方账号".to_string()),
            display_name: Some("Codex 官方账号".to_string()),
            account_id: Some("account-id".to_string()),
            ..Default::default()
        });

        assert_eq!(provider_label(&official_provider), "Codex 官方账号");
    }
}
