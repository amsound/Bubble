//! Migration report, written as `<name>.import-report.json` and `.md`.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub source: Source,
    pub summary: Summary,
    pub carts: Vec<CartReport>,
    /// Session-level settings that were not carried over.
    #[serde(default)]
    pub session_unsupported: Vec<Unsupported>,
    #[serde(default)]
    pub session_warnings: Vec<String>,
    pub discarded_features: Vec<String>,
    pub manual_actions: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub file: String,
    pub format_version: i32,
    pub trailer_ok: bool,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct Summary {
    pub slots_total: usize,
    pub carts_imported: usize,
    pub assets_resolved: usize,
    pub assets_missing: usize,
    pub settings_unsupported: usize,
    pub features_discarded: usize,
    pub manual_actions: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CartStatus {
    Imported,
    ImportedMissingAsset,
    /// A label or comment without any audio file.
    ImportedNoAsset,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CartReport {
    /// 1-based reference button number.
    pub ref_button: u16,
    pub new_cart_id: Uuid,
    pub page: String,
    pub row: u16,
    pub col: u16,
    pub label: String,
    pub status: CartStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset: Option<AssetReport>,
    pub unsupported: Vec<Unsupported>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AssetReport {
    pub ref_path: String,
    /// Local path the asset was found at.
    pub resolved: Option<String>,
    /// How it was found: `search_root`, `drive_map` or `name_search`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    pub tried: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Unsupported {
    pub field: String,
    pub value: String,
    pub action: String,
}

impl Unsupported {
    pub fn dropped(field: &str, value: impl ToString) -> Self {
        Unsupported {
            field: field.into(),
            value: value.to_string(),
            action: "dropped".into(),
        }
    }
}

impl Report {
    pub fn to_json(&self) -> String {
        let mut s = serde_json::to_string_pretty(self).expect("report serialises");
        s.push('\n');
        s
    }

    pub fn to_markdown(&self) -> String {
        let s = &self.summary;
        let mut out = format!(
            "# Import report: {}\n\n\
             | | |\n|---|---|\n\
             | Format version | {} |\n| Trailer check | {} |\n| SHA-256 | `{}` |\n\
             | Slots in session | {} |\n| Carts imported | {} |\n\
             | Assets resolved / missing | {} / {} |\n| Settings not carried over | {} |\n\n",
            self.source.file,
            self.source.format_version,
            if self.source.trailer_ok {
                "ok"
            } else {
                "FAILED"
            },
            self.source.sha256,
            s.slots_total,
            s.carts_imported,
            s.assets_resolved,
            s.assets_missing,
            s.settings_unsupported,
        );
        if !self.manual_actions.is_empty() {
            out.push_str("## Manual actions\n\n");
            for a in &self.manual_actions {
                out.push_str(&format!("- {a}\n"));
            }
            out.push('\n');
        }
        if !self.session_warnings.is_empty() || !self.session_unsupported.is_empty() {
            out.push_str("## Session\n\n");
            for w in &self.session_warnings {
                out.push_str(&format!("- {w}\n"));
            }
            for u in &self.session_unsupported {
                out.push_str(&format!("- `{}` = {} ({})\n", u.field, u.value, u.action));
            }
            out.push('\n');
        }
        if !self.discarded_features.is_empty() {
            out.push_str("## Discarded features\n\n");
            for f in &self.discarded_features {
                out.push_str(&format!("- {f}\n"));
            }
            out.push('\n');
        }
        out.push_str("## Carts\n\n| Button | Page | Row, col | Label | Status | File |\n|---|---|---|---|---|---|\n");
        for c in &self.carts {
            let file = c.asset.as_ref().map_or(String::new(), |a| {
                a.resolved
                    .clone()
                    .unwrap_or_else(|| format!("missing: `{}`", a.ref_path))
            });
            out.push_str(&format!(
                "| {} | {} | {}, {} | {} | {} | {} |\n",
                c.ref_button,
                md_cell(&c.page),
                c.row + 1,
                c.col + 1,
                md_cell(&c.label),
                serde_json::to_value(c.status)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default(),
                md_cell(&file),
            ));
        }
        let noted: Vec<_> = self
            .carts
            .iter()
            .filter(|c| !c.warnings.is_empty() || !c.unsupported.is_empty())
            .collect();
        if !noted.is_empty() {
            out.push_str("\n## Cart notes\n\n");
            for c in noted {
                out.push_str(&format!("### Button {}\n\n", c.ref_button));
                for w in &c.warnings {
                    out.push_str(&format!("- {w}\n"));
                }
                for u in &c.unsupported {
                    out.push_str(&format!("- `{}` = {} ({})\n", u.field, u.value, u.action));
                }
                out.push('\n');
            }
        }
        out
    }
}

fn md_cell(s: &str) -> String {
    s.replace('|', "\\|").replace(['\r', '\n'], " ")
}
