use clap::{Args, Parser, Subcommand, ValueEnum};
use codex_companion_core::{
    default_codex_dir, default_refresh_interval_seconds, ApiClientCreate, ApiClientUpdate,
    GroupPolicy, ProviderKind, RelaySettingsUpdate, RepairOptions,
};
use codex_companion_daemon::CompanionDaemon;
use codex_companion_provider::{GroupUpsert, ProviderUpsert};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "codex-companion",
    version,
    about = "Codex local provider runtime"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Install(PathOpt),
    Uninstall(PathOpt),
    Doctor(PathOpt),
    Status,
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    Relay {
        #[command(subcommand)]
        command: RelayCommand,
    },
    Provider {
        #[command(subcommand)]
        command: ProviderCommand,
    },
    Group {
        #[command(subcommand)]
        command: GroupCommand,
    },
    Repair(RepairArgs),
    TokenStats(TokenStatsArgs),
    Sessions(SessionsArgs),
}

#[derive(Debug, Subcommand)]
enum DaemonCommand {
    Start,
}

#[derive(Debug, Subcommand)]
enum RelayCommand {
    Start,
    Status,
    SelfTest,
    Client {
        #[command(subcommand)]
        command: RelayClientCommand,
    },
    Logs {
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    ClearLogs,
    Settings(RelaySettingsArgs),
}

#[derive(Debug, Subcommand)]
enum RelayClientCommand {
    Create(RelayClientCreateArgs),
    List,
    Update(RelayClientUpdateArgs),
    Rotate { id: String },
    Delete { id: String },
}

#[derive(Debug, Args)]
struct RelayClientCreateArgs {
    #[arg(long)]
    name: String,
    #[arg(long, default_value = "")]
    models: String,
}

#[derive(Debug, Args)]
struct RelayClientUpdateArgs {
    #[arg(long)]
    id: String,
    #[arg(long)]
    name: Option<String>,
    #[arg(long)]
    models: Option<String>,
    #[arg(long)]
    enabled: Option<bool>,
}

#[derive(Debug, Args)]
struct RelaySettingsArgs {
    #[arg(long)]
    require_api_key: Option<bool>,
    #[arg(long)]
    retry_budget: Option<u16>,
    #[arg(long)]
    model_cooldown_seconds: Option<u64>,
    #[arg(long)]
    session_affinity_ttl_seconds: Option<u64>,
    #[arg(long)]
    request_log_retention_days: Option<u16>,
}

#[derive(Debug, Subcommand)]
enum ProviderCommand {
    Add(ProviderAddArgs),
    Import(ProviderImportArgs),
    ImportLocal(PathOpt),
    List,
    Remove { id: String },
    Test { id: String },
    Refresh { id: String },
    RefreshAll,
}

#[derive(Debug, Subcommand)]
enum GroupCommand {
    Create(GroupCreateArgs),
    List,
    Use { id: String },
    Set(GroupSetArgs),
}

#[derive(Debug, Args)]
struct PathOpt {
    #[arg(long)]
    codex_dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct TokenStatsArgs {
    #[arg(long)]
    codex_dir: Option<PathBuf>,
    #[arg(long)]
    start_date: Option<String>,
    #[arg(long)]
    end_date: Option<String>,
    #[arg(long)]
    provider: Option<String>,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    rebuild: bool,
}

#[derive(Debug, Args)]
struct SessionsArgs {
    #[arg(long)]
    codex_dir: Option<PathBuf>,
    #[arg(long)]
    query: Option<String>,
    #[arg(long, default_value_t = 50)]
    limit: usize,
    #[arg(long)]
    rebuild: bool,
}

#[derive(Debug, Args)]
struct ProviderAddArgs {
    #[arg(long)]
    id: String,
    #[arg(long)]
    name: String,
    #[arg(long, value_enum)]
    kind: ProviderKindArg,
    #[arg(long)]
    base_url: String,
    #[arg(long)]
    auth_ref: Option<String>,
    #[arg(long, default_value_t = 100)]
    priority: i32,
    #[arg(long, default_value_t = true)]
    enabled: bool,
    #[arg(long, default_value_t = default_refresh_interval_seconds())]
    refresh_interval_seconds: u64,
}

#[derive(Debug, Args)]
struct ProviderImportArgs {
    #[arg(long)]
    json_file: PathBuf,
    #[arg(long)]
    provider_id: Option<String>,
    #[arg(long)]
    provider_name: Option<String>,
}

#[derive(Debug, Args)]
struct GroupCreateArgs {
    #[arg(long)]
    id: String,
    #[arg(long)]
    name: String,
    #[arg(long, value_delimiter = ',')]
    providers: Vec<String>,
    #[arg(long, value_enum, default_value_t = GroupPolicyArg::PriorityFallback)]
    policy: GroupPolicyArg,
    #[arg(long, default_value_t = true)]
    fallback: bool,
}

#[derive(Debug, Args)]
struct GroupSetArgs {
    #[arg(long)]
    id: String,
    #[arg(long, value_delimiter = ',')]
    providers: Vec<String>,
}

#[derive(Debug, Args)]
struct RepairArgs {
    #[arg(long)]
    history: bool,
    #[arg(long)]
    plugins: bool,
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    codex_dir: Option<PathBuf>,
    #[arg(long)]
    target_provider_id: Option<String>,
}

#[derive(Debug, Clone, ValueEnum)]
enum ProviderKindArg {
    OfficialCodex,
    OpenaiCompatible,
    RelayProvider,
}

#[derive(Debug, Clone, ValueEnum)]
enum GroupPolicyArg {
    PriorityFallback,
    Manual,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let daemon = CompanionDaemon::default()?;

    match cli.command {
        Command::Install(args) => print_json(daemon.install(args.codex_dir)?)?,
        Command::Uninstall(args) => print_json(daemon.uninstall(args.codex_dir)?)?,
        Command::Doctor(args) => print_json(daemon.doctor(args.codex_dir)?)?,
        Command::Status => print_json(daemon.status()?)?,
        Command::Daemon {
            command: DaemonCommand::Start,
        } => {
            let status = daemon.status()?;
            eprintln!(
                "Codex Companion relay listening at {}",
                status.relay_base_url
            );
            daemon.start_relay().await?;
        }
        Command::Relay { command } => match command {
            RelayCommand::Start => {
                let status = daemon.status()?;
                eprintln!(
                    "Codex Companion relay listening at {}",
                    status.relay_base_url
                );
                daemon.start_relay().await?;
            }
            RelayCommand::Status => {
                let self_test = daemon.api_service_self_test().await;
                let snapshot = daemon.api_service_snapshot()?;
                print_json(serde_json::json!({
                    "selfTest": self_test,
                    "snapshot": snapshot,
                }))?;
            }
            RelayCommand::SelfTest => print_json(daemon.api_service_self_test().await)?,
            RelayCommand::Client { command } => match command {
                RelayClientCommand::Create(args) => {
                    print_json(daemon.create_api_client(ApiClientCreate {
                        name: args.name,
                        allowed_models: parse_models(&args.models),
                    })?)?;
                }
                RelayClientCommand::List => {
                    print_json(daemon.api_service_snapshot()?.clients)?;
                }
                RelayClientCommand::Update(args) => {
                    let current = daemon
                        .api_service_snapshot()?
                        .clients
                        .into_iter()
                        .find(|client| client.id == args.id)
                        .ok_or_else(|| anyhow::anyhow!("unknown API client: {}", args.id))?;
                    print_json(
                        daemon.update_api_client(ApiClientUpdate {
                            id: current.id,
                            name: args.name.unwrap_or(current.name),
                            allowed_models: args
                                .models
                                .as_deref()
                                .map(parse_models)
                                .unwrap_or(current.allowed_models),
                            enabled: args.enabled.unwrap_or(current.enabled),
                        })?,
                    )?;
                }
                RelayClientCommand::Rotate { id } => {
                    print_json(daemon.rotate_api_client_key(&id)?)?;
                }
                RelayClientCommand::Delete { id } => {
                    print_json(daemon.delete_api_client(&id)?)?;
                }
            },
            RelayCommand::Logs { limit } => {
                let requests = daemon.api_service_snapshot()?.recent_requests;
                print_json(
                    requests
                        .into_iter()
                        .take(limit.clamp(1, 100))
                        .collect::<Vec<_>>(),
                )?;
            }
            RelayCommand::ClearLogs => print_json(daemon.clear_api_request_logs()?)?,
            RelayCommand::Settings(args) => {
                let current = daemon.status()?.config.relay;
                print_json(
                    daemon.update_relay_settings(RelaySettingsUpdate {
                        require_api_key: args.require_api_key.unwrap_or(current.require_api_key),
                        retry_budget: args.retry_budget.unwrap_or(current.retry_budget),
                        model_cooldown_seconds: args
                            .model_cooldown_seconds
                            .unwrap_or(current.model_cooldown_seconds),
                        session_affinity_ttl_seconds: args
                            .session_affinity_ttl_seconds
                            .unwrap_or(current.session_affinity_ttl_seconds),
                        request_log_retention_days: args
                            .request_log_retention_days
                            .unwrap_or(current.request_log_retention_days),
                    })?,
                )?;
            }
        },
        Command::Provider { command } => match command {
            ProviderCommand::Add(args) => {
                let provider = daemon.add_provider(ProviderUpsert {
                    id: args.id,
                    name: args.name,
                    kind: args.kind.into(),
                    base_url: args.base_url,
                    auth_ref: args.auth_ref,
                    direct_auth_ref: None,
                    model_map: BTreeMap::new(),
                    priority: args.priority,
                    enabled: args.enabled,
                    refresh_interval_seconds: args.refresh_interval_seconds,
                    account: None,
                })?;
                print_json(provider)?;
            }
            ProviderCommand::Import(args) => {
                let json_text = fs::read_to_string(&args.json_file)?;
                print_json(daemon.import_provider_json_many(
                    &json_text,
                    args.provider_id,
                    args.provider_name,
                )?)?;
            }
            ProviderCommand::ImportLocal(args) => {
                print_json(daemon.import_local_codex_provider(args.codex_dir)?)?;
            }
            ProviderCommand::List => print_json(daemon.list_providers()?)?,
            ProviderCommand::Remove { id } => print_json(daemon.remove_provider(&id)?)?,
            ProviderCommand::Test { id } => match daemon.test_provider(&id).await {
                Ok(()) => println!("ok"),
                Err(error) => anyhow::bail!(error),
            },
            ProviderCommand::Refresh { id } => print_json(daemon.refresh_provider(&id).await?)?,
            ProviderCommand::RefreshAll => print_json(daemon.refresh_all_providers().await?)?,
        },
        Command::Group { command } => match command {
            GroupCommand::Create(args) => {
                let group = daemon.upsert_group(GroupUpsert {
                    id: args.id,
                    name: args.name,
                    policy: args.policy.into(),
                    provider_order: args.providers,
                    fallback_enabled: args.fallback,
                })?;
                print_json(group)?;
            }
            GroupCommand::List => print_json(daemon.status()?.config.groups)?,
            GroupCommand::Use { id } => print_json(daemon.use_group(&id)?)?,
            GroupCommand::Set(args) => {
                print_json(daemon.set_group_order(&args.id, args.providers)?)?
            }
        },
        Command::Repair(args) => {
            let codex_dir = match args.codex_dir {
                Some(path) => path,
                None => default_codex_dir()?,
            };
            print_json(daemon.repair(RepairOptions {
                codex_dir,
                history: args.history,
                plugins: args.plugins,
                dry_run: args.dry_run,
                target_provider_id: args.target_provider_id,
            })?)?;
        }
        Command::TokenStats(args) => {
            let codex_dir = match args.codex_dir {
                Some(path) => path,
                None => default_codex_dir()?,
            };
            print_json(daemon.token_usage_filtered(
                codex_dir,
                args.start_date.as_deref(),
                args.end_date.as_deref(),
                args.provider.as_deref(),
                args.model.as_deref(),
                args.rebuild,
            )?)?;
        }
        Command::Sessions(args) => {
            let codex_dir = match args.codex_dir {
                Some(path) => path,
                None => default_codex_dir()?,
            };
            print_json(daemon.session_page(
                codex_dir,
                args.query.as_deref(),
                args.limit,
                args.rebuild,
            )?)?;
        }
    }

    Ok(())
}

fn print_json(value: impl serde::Serialize) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn parse_models(value: &str) -> Vec<String> {
    value
        .split([',', '\n'])
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .fold(Vec::new(), |mut models, model| {
            if !models.iter().any(|known| known == model) {
                models.push(model.to_string());
            }
            models
        })
}

impl From<ProviderKindArg> for ProviderKind {
    fn from(value: ProviderKindArg) -> Self {
        match value {
            ProviderKindArg::OfficialCodex => Self::OfficialCodex,
            ProviderKindArg::OpenaiCompatible => Self::OpenAiCompatible,
            ProviderKindArg::RelayProvider => Self::RelayProvider,
        }
    }
}

impl From<GroupPolicyArg> for GroupPolicy {
    fn from(value: GroupPolicyArg) -> Self {
        match value {
            GroupPolicyArg::PriorityFallback => Self::PriorityFallback,
            GroupPolicyArg::Manual => Self::Manual,
        }
    }
}
