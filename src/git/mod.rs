pub mod binary_policy;
pub mod branch_features;
pub mod branch_policy;
pub mod catalog_apply;
pub mod catalog_build;
pub mod chat_index;
pub mod clever_hooks;
pub mod clever_script_host;
pub mod embed_html;
pub mod get_hook_http;
pub mod git_proxy;
pub mod hooks;
pub mod image_cache;
pub mod image_transform;
pub mod index_policy;
pub mod index_store;
pub mod indexing;
pub mod ipfs;
pub mod markdown_html;
pub mod meta_index;
pub mod origins;
pub mod portrait;
pub mod privileged_policy;
pub mod query;
pub mod query_cache;
pub mod relay_config_cache;
pub mod repo;
pub mod request_headers;
pub mod resolve;
pub mod search_config;
pub mod search_plugins;
pub mod search_store;
pub mod seo_from_markdown;
pub mod sqlite_index;
pub mod ssr_client_hook;
pub mod ssr_html_cache;
pub mod web_assets;
pub mod worktree;

#[cfg(test)]
mod tests;

pub use branch_policy::{
    branch_requires_signature, enforce_all_commits_on_branch, enforce_publish_origin_fence,
    enforce_push_range, enforce_push_range_with_catalog, enforce_single_commit,
    enforce_unsigned_http_allowed, publish_origin_push_gate, resolve_branch_rule,
};
pub use get_hook_http::get_hook_json_to_response;
pub use hooks::{execute_repo_hook, HookContext};
pub use index_policy::has_index_hook;
pub use indexing::{
    branch_data_dir, branch_hash_short, branch_index_paths, branch_operation_mutex,
    branch_sqlite_path, ensure_indexed, ensure_indexed_force, reconcile_stale_indexes,
    spawn_ensure_indexed, spawn_periodic_reconcile_stale_indexes, spawn_reconcile_stale_indexes,
};
pub use repo::{
    apply_bare_receive_security, apply_bare_receive_security_at_path, bare_repo_names,
    default_branch_from_bare_head, ensure_bare_repo, get_branch_commit_info, list_branches,
    open_repo, open_repo_for_state, read_file_from_repo, read_git_config,
    read_git_config_from_revision, read_relay_config, repo_names_for_state,
};
pub use resolve::{
    git_resolve_and_respond, try_site_shell_for_root, wants_relay_tree, RELAY_TREE_MEDIA_TYPE,
};
pub use search_store::{search_db_path, store_for_bare_repo, store_key_for_bare};
