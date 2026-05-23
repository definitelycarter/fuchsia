use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tokio_util::sync::CancellationToken;

use fuchsia_component_registry::FsComponentRegistry;
use fuchsia_config::WorkflowDef;
use fuchsia_resolver::{Resolver, StandardResolver};
use fuchsia_task_runtime::{RuntimeRegistry, RuntimeType};
use fuchsia_task_runtime_wasm::WasmExecutor;
use fuchsia_workflow_orchestrator::{Orchestrator, OrchestratorConfig};

/// Fuchsia - A workflow engine built on WebAssembly components
#[derive(Parser)]
#[command(name = "fuchsia")]
#[command(version, about, long_about = None)]
struct Cli {
  /// Path to the data directory (default: ~/.fuchsia)
  #[arg(long, global = true)]
  data_dir: Option<PathBuf>,

  #[command(subcommand)]
  command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
  /// Run a workflow or task
  Run {
    #[command(subcommand)]
    target: RunTarget,
  },
}

#[derive(Subcommand)]
enum RunTarget {
  /// Run an entire workflow
  Workflow {
    /// Path to the workflow file (JSON or YAML)
    workflow_file: PathBuf,
  },

  /// Run a single node from a workflow
  Node {
    /// Path to the workflow file (JSON or YAML)
    workflow_file: PathBuf,

    /// The node ID to execute
    #[arg(long)]
    node: String,
  },
}

fn main() -> Result<()> {
  let cli = Cli::parse();

  let data_dir = cli.data_dir.unwrap_or_else(|| {
    dirs::home_dir()
      .expect("could not determine home directory")
      .join(".fuchsia")
  });

  match cli.command {
    Some(Commands::Run { target }) => match target {
      RunTarget::Workflow { workflow_file } => {
        run_workflow(workflow_file, data_dir)?;
      }
      RunTarget::Node {
        workflow_file,
        node,
      } => {
        run_node(workflow_file, node, data_dir)?;
      }
    },
    None => {
      println!("fuchsia - use --help to see available commands");
    }
  }

  Ok(())
}

fn run_workflow(workflow_file: PathBuf, data_dir: PathBuf) -> Result<()> {
  let rt = tokio::runtime::Runtime::new()?;
  rt.block_on(async { run_workflow_async(workflow_file, data_dir).await })
}

async fn run_workflow_async(workflow_file: PathBuf, data_dir: PathBuf) -> Result<()> {
  let workflow_content = tokio::fs::read_to_string(&workflow_file)
    .await
    .with_context(|| format!("failed to read workflow file: {}", workflow_file.display()))?;

  let workflow_def: WorkflowDef = serde_json::from_str(&workflow_content)
    .with_context(|| format!("failed to parse workflow file: {}", workflow_file.display()))?;

  eprintln!("Loaded workflow: {}", workflow_def.name);

  let payload = read_payload_from_stdin()?;
  eprintln!("Payload: {}", payload);

  let components_dir = data_dir.join("components");
  let registry = FsComponentRegistry::new(&components_dir);

  let resolver = StandardResolver::new(registry);
  let workflow = resolver
    .resolve(workflow_def)
    .await
    .context("failed to resolve workflow")?;

  eprintln!("Resolved workflow with {} nodes", workflow.nodes.len());

  let mut runtime_registry = RuntimeRegistry::new();
  runtime_registry.register(RuntimeType::Wasm, Arc::new(WasmExecutor::new(Default::default()).expect("failed to create wasm executor")));

  let component_registry = Arc::new(FsComponentRegistry::new(&components_dir));
  let orchestrator = Orchestrator::new(
    workflow,
    Arc::new(runtime_registry),
    component_registry,
    OrchestratorConfig::default(),
  )
  .context("failed to create orchestrator")?;

  let cancel = CancellationToken::new();
  let result = orchestrator
    .invoke(payload, cancel)
    .await
    .context("workflow execution failed")?;

  eprintln!("Execution completed: {}", result.execution_id);
  eprintln!("Nodes executed: {}", result.node_results.len());

  let output: serde_json::Map<String, serde_json::Value> = result
    .node_results
    .into_iter()
    .map(|(id, r)| (id, r.output))
    .collect();

  println!("{}", serde_json::to_string_pretty(&output)?);

  Ok(())
}

fn run_node(workflow_file: PathBuf, node: String, data_dir: PathBuf) -> Result<()> {
  let rt = tokio::runtime::Runtime::new()?;
  rt.block_on(async { run_node_async(workflow_file, node, data_dir).await })
}

async fn run_node_async(workflow_file: PathBuf, node_id: String, data_dir: PathBuf) -> Result<()> {
  let workflow_content = tokio::fs::read_to_string(&workflow_file)
    .await
    .with_context(|| format!("failed to read workflow file: {}", workflow_file.display()))?;

  let workflow_def: WorkflowDef = serde_json::from_str(&workflow_content)
    .with_context(|| format!("failed to parse workflow file: {}", workflow_file.display()))?;

  eprintln!("Running node: {}", node_id);

  let payload = read_payload_from_stdin()?;
  eprintln!("Payload: {}", payload);

  let components_dir = data_dir.join("components");
  let registry = FsComponentRegistry::new(&components_dir);

  let resolver = StandardResolver::new(registry);
  let workflow = resolver
    .resolve(workflow_def)
    .await
    .context("failed to resolve workflow")?;

  let mut runtime_registry = RuntimeRegistry::new();
  runtime_registry.register(RuntimeType::Wasm, Arc::new(WasmExecutor::new(Default::default()).expect("failed to create wasm executor")));

  let component_registry = Arc::new(FsComponentRegistry::new(&components_dir));
  let orchestrator = Orchestrator::new(
    workflow,
    Arc::new(runtime_registry),
    component_registry,
    OrchestratorConfig::default(),
  )
  .context("failed to create orchestrator")?;

  let cancel = CancellationToken::new();
  let result = orchestrator
    .invoke_node(&node_id, payload, cancel)
    .await
    .context("node execution failed")?;

  eprintln!("Node '{}' completed", result.node_id);

  println!("{}", serde_json::to_string_pretty(&result.output)?);

  Ok(())
}

fn read_payload_from_stdin() -> Result<serde_json::Value> {
  use std::io::IsTerminal;

  if io::stdin().is_terminal() {
    Ok(serde_json::json!({}))
  } else {
    let mut input = String::new();
    io::stdin()
      .read_to_string(&mut input)
      .context("failed to read payload from stdin")?;

    if input.trim().is_empty() {
      Ok(serde_json::json!({}))
    } else {
      serde_json::from_str(&input).context("failed to parse payload JSON from stdin")
    }
  }
}
