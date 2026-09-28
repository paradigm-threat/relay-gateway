pub mod accept;
pub mod file;
pub mod general;
pub mod head;
pub mod helpers;
pub mod ipfs;
pub mod mcp;
pub mod mesh_sync;
pub mod node_api;
pub mod query;
pub mod query_response;
pub mod rum;
pub mod status;
pub mod write;

pub use crate::mesh_events::post_mesh_event;
pub use crate::peer_hook::post_peer_hook;
pub use file::{handle_get_file, try_static};
pub use general::{
    api_config, get_openapi_yaml, get_root, get_swagger_ui, options_capabilities, post_api_config,
    post_ensure_bare, post_git_pull, post_github_hook, post_index_warm, serve_acme_challenge,
};
pub use head::{head_file, head_root};
pub use ipfs::{handle_get_ipfs, handle_get_ipfs_prefixed};
pub use mesh_sync::api_mesh_sync;
pub use query::handle_query;
pub use rum::api_rum;
pub use status::{api_status, post_api_status};
pub use write::{delete_file, put_file};

/// Sync SSR and git resolve must not sit on the Tokio accept path.
/// `block_in_place` is the production path (multi-thread runtime). Current-thread
/// runtimes — unit tests — panic inside `block_in_place`, so call `f` inline there.
pub(crate) fn run_off_accept<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    let multi = tokio::runtime::Handle::try_current()
        .map(|h| h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread)
        .unwrap_or(false);
    if multi {
        tokio::task::block_in_place(f)
    } else {
        f()
    }
}
