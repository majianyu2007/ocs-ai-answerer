use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use ocs_ai_answerer::api::ocs_config::answerer_wrapper;
use ocs_ai_answerer::config::{Config, ConfigSource, DEFAULT_CONFIG_PATH};
use ocs_ai_answerer::{api, build_state};

#[derive(Debug, Parser)]
#[command(
    name = "ocs-ai-answerer",
    version,
    about = "基于大语言模型的 OCS 题库服务"
)]
struct Cli {
    /// 配置文件路径（默认 ./config.toml，也可用环境变量 OCS_CONFIG 指定）
    #[arg(short, long, env = "OCS_CONFIG")]
    config: Option<PathBuf>,
    /// 覆盖监听地址
    #[arg(long)]
    host: Option<String>,
    /// 覆盖监听端口
    #[arg(short, long)]
    port: Option<u16>,
    /// 只校验配置后退出
    #[arg(long)]
    check: bool,
    /// 打印 OCS 题库配置 JSON 后退出
    #[arg(long)]
    print_ocs_config: bool,
}

fn main() -> ExitCode {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();

    let explicit = cli.config.is_some();
    let path = cli
        .config
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH));
    let (mut config, source) = match Config::load(&path, explicit) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("启动失败: {e}");
            if !path.exists() {
                eprintln!(
                    "提示: 复制 config.example.toml 为 config.toml 并填写 [ai] 中的 api_key / model，或通过环境变量 OCS_AI_API_KEY、OCS_AI_MODEL 配置。"
                );
            }
            return ExitCode::FAILURE;
        }
    };
    if let Some(host) = cli.host {
        config.server.host = host;
    }
    if let Some(port) = cli.port {
        config.server.port = port;
    }

    if cli.print_ocs_config {
        let base = if config.server.public_url.is_empty() {
            format!("http://127.0.0.1:{}", config.server.port)
        } else {
            config.server.public_url.clone()
        };
        let token =
            (!config.server.access_token.is_empty()).then_some(config.server.access_token.as_str());
        let json = answerer_wrapper(&base, "post", token);
        println!(
            "{}",
            serde_json::to_string_pretty(&json).unwrap_or_default()
        );
        return ExitCode::SUCCESS;
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("RUST_LOG")
                .unwrap_or_else(|_| EnvFilter::new(&config.server.log_level)),
        )
        .with_target(false)
        .init();

    match &source {
        ConfigSource::File(p) => tracing::info!(path = %p.display(), "已加载配置文件"),
        ConfigSource::DefaultsAndEnv => {
            tracing::info!("未找到 config.toml，使用默认值与环境变量配置")
        }
    }
    for w in config.warnings() {
        tracing::warn!("{w}");
    }

    if cli.check {
        println!("配置校验通过");
        return ExitCode::SUCCESS;
    }

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("无法启动异步运行时: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(serve(config)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e}");
            ExitCode::FAILURE
        }
    }
}

async fn serve(config: Config) -> Result<(), String> {
    let host = config.server.host.clone();
    let port = config.server.port;
    let state = build_state(config).map_err(|e| e.to_string())?;
    let client = state.answerer.client();
    tracing::info!(
        provider = %client.kind(),
        model = client.model(),
        endpoint = %client.endpoint(),
        "AI 接口已配置（尚未实际连通，可访问 /health?check=true 检测）"
    );

    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .or_else(|_| format!("[{host}]:{port}").parse())
        .map_err(|e| format!("监听地址无效 {host}:{port}: {e}"))?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("无法监听 {addr}: {e}（端口可能被占用）"))?;

    let display_host = if addr.ip().is_unspecified() {
        "127.0.0.1".to_string()
    } else {
        addr.ip().to_string()
    };
    tracing::info!("服务已启动: http://{display_host}:{port}");
    tracing::info!("OCS 题库配置: http://{display_host}:{port}/ocs-config");

    axum::serve(listener, api::router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| format!("服务异常退出: {e}"))
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("收到退出信号，正在关闭服务");
}
