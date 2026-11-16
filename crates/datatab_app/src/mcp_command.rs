#[cfg(feature = "mcp")]
pub fn run_mcp_command(client_id: String, config_dir: Option<std::path::PathBuf>) -> i32 {
    let args = datatab_mcp_server::McpServerArgs {
        client_id,
        config_dir,
    };
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("Failed to create tokio runtime: {error}");
            return 1;
        }
    };
    match runtime.block_on(datatab_mcp_server::run_mcp_server(args)) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("MCP server error: {error}");
            1
        }
    }
}

#[cfg(not(feature = "mcp"))]
pub fn run_mcp_command(_client_id: String, _config_dir: Option<std::path::PathBuf>) -> i32 {
    eprintln!("Error: MCP support not compiled into this build.");
    eprintln!();
    eprintln!("To enable MCP functionality, rebuild with:");
    eprintln!("  cargo build --features mcp");
    eprintln!();
    eprintln!("Or use the default build (MCP included):");
    eprintln!("  cargo build");
    1
}
