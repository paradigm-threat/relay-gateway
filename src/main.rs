use anyhow::Result;
use clap::Parser;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tracing::{error, info};
use tracing_appender::rolling;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt};

use relay_server::{
    build_router,
    cli::Cli,
    config::{self, Config},
    notify,
};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Set up logging: stdout + rolling file appender
    let _ = std::fs::create_dir_all("logs");
    let file_appender = rolling::daily("logs", "server.log");
    let (file_nb, _guard) = tracing_appender::non_blocking(file_appender);
    let env_filter = tracing_subscriber::EnvFilter::from_default_env();
    let stdout_layer = fmt::layer()
        .with_target(true)
        .with_thread_ids(false)
        .with_thread_names(false)
        .compact();
    let file_layer = fmt::layer()
        .with_writer(file_nb)
        .with_target(true)
        .compact();
    tracing_subscriber::registry()
        .with(env_filter)
        .with(stdout_layer)
        .with(file_layer)
        .init();

    notify::install_panic_hook();

    let config = Config::from_cli(&cli)?;
    config.initialize_repos();

    if let Some(relay_server::cli::Commands::Query(args)) = cli.command {
        let query_val = args.query.map(serde_json::Value::String);
        match relay_server::git::query::execute_query(
            &config.state.repo_path,
            &args.repo,
            &args.branch,
            query_val,
            &args.collection,
        ) {
            Ok(results) => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({ "results": results }))?
                );
                return Ok(());
            }
            Err(e) => {
                error!("Query failed: {}", e);
                std::process::exit(1);
            }
        }
    }

    info!(repo_path = %config.state.repo_path.display(), "Repository path resolved");
    let mesh_role = relay_server::mesh_role::relay_node_role();
    info!(
        ?mesh_role,
        mesh_http_publish = relay_server::mesh_role::should_publish_mesh_http_events(),
        "relay mesh node role"
    );

    relay_server::park_tls::spawn_worker(config.state.clone());

    let app = build_router(config.state.clone());

    let http_addr = config.http_addr;
    let app_http = app.clone();
    let started_node = config.state.node_fqdn.clone();
    let started_sid = config.state.relay_server_id.clone();
    let http_task = tokio::spawn(async move {
        info!(%http_addr, "HTTP listening");
        let listener = TcpListener::bind(&http_addr).await.expect("bind http");
        notify::notify_server_started(started_node.as_deref(), started_sid.as_deref());
        if let Err(e) = axum::serve(listener, app_http.into_make_service()).await {
            error!(?e, "HTTP server error");
        }
    });

    // HTTPS optional
    let https_task = if let (Some(cert_path), Some(key_path)) = (config.tls_cert, config.tls_key) {
        let https_addr: SocketAddr = format!("0.0.0.0:{}", config.https_port).parse()?;
        let tls_config = config::load_rustls_config(&cert_path, &key_path).await?;
        let app_https = app;
        Some(tokio::spawn(async move {
            info!(%https_addr, cert=%cert_path, key=%key_path, "HTTPS listening");
            if let Err(e) = axum_server::bind_rustls(https_addr, tls_config)
                .serve(app_https.into_make_service())
                .await
            {
                error!(?e, "HTTPS server error");
            }
        }))
    } else {
        info!("TLS is disabled: RELAY_TLS_CERT and RELAY_TLS_KEY not both set");
        None
    };

    if let Some(t) = https_task {
        let _ = tokio::join!(http_task, t);
    } else {
        let _ = tokio::join!(http_task);
    }
    Ok(())
}
