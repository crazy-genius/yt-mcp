use crate::mcp::YoutrackMCPServer;
use rmcp::model::Tool;

pub mod mcp;
mod references;

pub fn extract_tools(mcp: &YoutrackMCPServer) -> Vec<Tool> {
    mcp.tool_router.list_all()
}
