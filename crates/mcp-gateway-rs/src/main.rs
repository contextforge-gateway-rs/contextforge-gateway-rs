mod logging;
mod runtime;

use std::{sync::Arc, thread};

use clap::Parser;

use mcp_gateway_rs_lib::Config;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use tracing::{debug, error, info, warn};

use crate::runtime::RuntimeType;

use tikv_jemallocator::Jemalloc;

#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;
#[allow(clippy::print_stdout)]
fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = Config::parse();
    println!("mcp-gateway-rs {config:?}");
    let _guard = logging::init_tracing_logging(&config);

    let builder = runtime::RuntimeBuilder::from(&config);

    let local_session_manager = Arc::new(LocalSessionManager::default());

    match builder.build() {
        RuntimeType::SingleThreaded(runtimes) => {
            if runtimes.iter().any(std::result::Result::is_err) {
                Err("Couldn't start all single threaded runtimes".into())
            } else {
                let join_handles: Vec<_> = runtimes
                    .into_iter()
                    .flatten()
                    .map(|r| {
                        let config = config.clone();
                        let local_session_manager = Arc::clone(&local_session_manager);

                        thread::Builder::new().name("mcp-gateway-rs".to_owned()).spawn(move || {
                            r.block_on(async {
                                tokio::select! {
                                    res = mcp_gateway_rs_lib::run_gateway(config,local_session_manager) => {
                                        if res.is_ok(){
                                            debug!("Gateway process terminated");
                                        }else{
                                            error!("Gateway process terminated {res:?}");
                                        }
                                    }
                                }
                            });
                        })
                    })
                    .collect();

                if join_handles.iter().any(std::result::Result::is_err) {
                    return Err("Couldn't start runtimes on threads".into());
                }
                join_handles.into_iter().flatten().for_each(|jh| {
                    if let Err(e) = jh.join() {
                        warn!("Thread handle terminated with error {e:?}");
                    }
                });

                Ok(())
            }
        },
        RuntimeType::MultiThreaded(runtime) => {
            if let Ok(runtime) = runtime {
                runtime.block_on(async {
                    tokio::select! {
                        _ = mcp_gateway_rs_lib::run_gateway(config,local_session_manager) => {
                        info!("Gateway Runtime Terminated!");
                        }
                    }
                });
            } else {
                return Err("Couldn't start multi threaded runtime".into());
            }
            Ok(())
        },
    }
}
