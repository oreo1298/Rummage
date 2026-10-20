//! JSON views of daemon status and file properties for QML (parsed with `JSON.parse`).

use fossick_client::Status;
use fossick_ui::desktop::Properties;
use serde_json::json;

/// `{entries, volumes: [{mount, fstype, label, state, method, entries, note}]}`.
#[must_use]
pub fn status_json(s: &Status) -> String {
    let volumes: Vec<serde_json::Value> = s
        .volumes
        .iter()
        .map(|v| {
            json!({
                "mount": String::from_utf8_lossy(&v.mount_path),
                "fstype": v.fstype,
                "label": v.label,
                "state": format!("{:?}", v.state),
                "method": format!("{:?}", v.method),
                "entries": v.entries,
                "note": v.note,
            })
        })
        .collect();
    json!({ "entries": s.entries, "volumes": volumes }).to_string()
}

/// Properties as JSON; `index_size` is the size the index knows (folder sizes).
#[must_use]
pub fn properties_json(p: &Properties, index_size: Option<u64>) -> String {
    json!({
        "path": p.path,
        "kind": p.kind,
        "size": if p.kind == "folder" { index_size.unwrap_or(0) } else { p.size },
        "sizeKnown": p.kind != "folder" || index_size.is_some(),
        "modified": p.modified,
        "accessed": p.accessed,
        "changed": p.changed,
        "created": p.created,
        "permissions": p.permissions,
        "mode": p.mode_octal,
        "owner": p.owner,
        "inode": p.inode,
        "links": p.links,
        "target": p.symlink_target,
    })
    .to_string()
}
