use std::{
    sync::{Arc, mpsc},
    thread,
};

use contextforge_data_plane_cpex::CpexRuntimeRegistry;
use contextforge_data_plane_lib::{Config, Gateway};
use tokio::runtime::{Builder, LocalOptions};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

#[derive(Debug, Clone)]
#[allow(clippy::struct_field_names)]
pub struct Runtime {
    single_runtime: bool,
    number_of_threads: usize,
    global_queue_interval: Option<u32>,
    event_interval: Option<u32>,
    max_io_events_per_tick: Option<usize>,
    thread_name: String,
}

impl<'b> From<&'b Config> for Runtime {
    fn from(config: &'b Config) -> Self {
        Self {
            single_runtime: config.single_runtime.unwrap_or(true),
            number_of_threads: config.number_of_cpus.unwrap_or(num_cpus::get()),
            ..Default::default()
        }
    }
}

impl Default for Runtime {
    fn default() -> Self {
        Self {
            single_runtime: true,
            number_of_threads: num_cpus::get(),
            global_queue_interval: Option::default(),
            event_interval: Option::default(),
            max_io_events_per_tick: Option::default(),
            thread_name: "contextforge-data-plane-runtime".to_owned(),
        }
    }
}

impl Runtime {
    fn configure_builder(&self, builder: &mut Builder, thread_name: String) {
        let builder = builder.enable_all().name(thread_name);
        let builder = if let Some(global_queue_interval) = self.global_queue_interval {
            builder.global_queue_interval(global_queue_interval)
        } else {
            builder
        };

        if let Some(event_interval) = self.event_interval {
            builder.event_interval(event_interval);
        }

        if let Some(max_io_events_per_tick) = self.max_io_events_per_tick {
            builder.max_io_events_per_tick(max_io_events_per_tick);
        }
    }

    fn configure_single_thread_builder(builder: &mut Builder, thread_name: String) {
        builder.enable_all().name(thread_name).global_queue_interval(1024).max_io_events_per_tick(4);
    }

    pub fn execute(
        self,
        gateway: Gateway,
        cpex_runtime: Option<Arc<CpexRuntimeRegistry>>,
    ) -> contextforge_data_plane_lib::Result<()> {
        if self.single_runtime {
            let mut builder = Builder::new_multi_thread();
            self.configure_builder(&mut builder, self.thread_name.clone());
            let runtime = builder.build()?;

            runtime.block_on(async {
                let _cpex_watcher = Self::initialize_cpex_runtime(cpex_runtime).await?;
                Self::run_gateway(gateway).await
            })
        } else {
            let (init_sender, init_receiver) = mpsc::channel();
            let mut handles = vec![Self::spawn_gateway_thread(
                format!("{}0", self.thread_name),
                gateway.clone(),
                cpex_runtime,
                Some(init_sender),
            )?];
            match init_receiver
                .recv()
                .map_err(|error| format!("CPEX plugin initialization result unavailable: {error}"))?
            {
                Ok(()) => {},
                Err(error) => return Err(error.into()),
            }

            for i in 1..self.number_of_threads {
                match Self::spawn_gateway_thread(format!("{}{i}", self.thread_name), gateway.clone(), None, None) {
                    Ok(handle) => handles.push(handle),
                    Err(error) => warn!(
                        component = "Runtime",
                        operation = "spawn_gateway_thread",
                        error = %error,
                        "gateway thread failed to start"
                    ),
                }
            }

            for handle in handles {
                let res = handle.join();
                info!(
                    component = "Runtime",
                    operation = "join_gateway_thread",
                    succeeded = res.is_ok(),
                    "gateway thread terminated"
                );
            }
            Ok(())
        }
    }

    fn spawn_gateway_thread(
        thread_name: String,
        gateway: Gateway,
        cpex_runtime: Option<Arc<CpexRuntimeRegistry>>,
        init_sender: Option<mpsc::Sender<std::result::Result<(), String>>>,
    ) -> std::io::Result<thread::JoinHandle<contextforge_data_plane_lib::Result<()>>> {
        thread::Builder::new().name(thread_name.clone()).spawn(move || {
            let mut builder = Builder::new_current_thread();
            Self::configure_single_thread_builder(&mut builder, thread_name);
            let runtime = match builder.build_local(LocalOptions::default()) {
                Ok(runtime) => runtime,
                Err(error) => {
                    warn!(
                        component = "Runtime",
                        operation = "build_gateway_thread",
                        error = %error,
                        "gateway runtime could not be built"
                    );
                    return Err::<(), contextforge_data_plane_lib::Error>(error.into());
                },
            };

            runtime.block_on(async {
                let Some(init_sender) = init_sender else {
                    return Self::run_gateway(gateway).await;
                };
                match Self::initialize_cpex_runtime(cpex_runtime).await {
                    Ok(cpex_watcher) => {
                        let _ = init_sender.send(Ok(()));
                        let _cpex_watcher = cpex_watcher;
                        Self::run_gateway(gateway).await
                    },
                    Err(error) => {
                        let _ = init_sender.send(Err(error.to_string()));
                        Err(error)
                    },
                }
            })
        })
    }

    async fn initialize_cpex_runtime(
        cpex_runtime: Option<Arc<CpexRuntimeRegistry>>,
    ) -> contextforge_data_plane_lib::Result<Option<JoinHandle<()>>> {
        let Some(cpex_runtime) = cpex_runtime else {
            return Ok(None);
        };
        match cpex_runtime.initialize().await {
            Ok(Some(handle)) => {
                debug!(component = "Plugins", operation = "initialize", "runtime plugins initialized");
                Ok(Some(handle))
            },
            Ok(None) => {
                debug!(component = "Plugins", operation = "initialize", "runtime plugin initialization skipped");
                Ok(None)
            },
            Err(error) => {
                error!(
                    component = "Plugins",
                    operation = "initialize",
                    error_code = "CFDP-PLUGIN-INIT",
                    root_cause = %error,
                    impact_scope = "service-startup",
                    retryable = false,
                    error = ?error,
                    "runtime plugin initialization failed"
                );
                Err(error)
            },
        }
    }

    async fn run_gateway(gateway: Gateway) -> contextforge_data_plane_lib::Result<()> {
        let res = gateway.run_gateway().await;
        if res.is_ok() {
            debug!(component = "Gateway", operation = "run", "gateway process terminated");
        } else {
            let error = res.as_ref().expect_err("checked error result");
            error!(
                component = "Gateway",
                operation = "run",
                error_code = "CFDP-GATEWAY-TERMINATED",
                root_cause = %error,
                impact_scope = "service-wide",
                retryable = true,
                error = ?error,
                "gateway process terminated unexpectedly"
            );
        }
        Ok(())
    }
}
