//! The result table’s columns.

use fossick_proto::SortField;
use serde::{Deserialize, Serialize};

/// A result column. The order of [`Column::ALL`] is the model’s column order; views reorder
/// them visually.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Column {
    Name,
    Path,
    Size,
    Modified,
    Created,
    Accessed,
    Extension,
    Type,
    Permissions,
    Owner,
    RunCount,
    Volume,
}

impl Column {
    pub const ALL: [Self; 12] = [
        Self::Name,
        Self::Path,
        Self::Size,
        Self::Modified,
        Self::Created,
        Self::Accessed,
        Self::Extension,
        Self::Type,
        Self::Permissions,
        Self::Owner,
        Self::RunCount,
        Self::Volume,
    ];

    /// Index in [`Column::ALL`].
    #[must_use]
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|&c| c == self).unwrap_or(0)
    }

    #[must_use]
    pub fn from_index(i: usize) -> Option<Self> {
        Self::ALL.get(i).copied()
    }

    /// Stable identifier (settings files, translations look titles up by it).
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Path => "path",
            Self::Size => "size",
            Self::Modified => "modified",
            Self::Created => "created",
            Self::Accessed => "accessed",
            Self::Extension => "extension",
            Self::Type => "type",
            Self::Permissions => "permissions",
            Self::Owner => "owner",
            Self::RunCount => "run-count",
            Self::Volume => "volume",
        }
    }

    /// English title (frontends translate it).
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Path => "Path",
            Self::Size => "Size",
            Self::Modified => "Date Modified",
            Self::Created => "Date Created",
            Self::Accessed => "Date Accessed",
            Self::Extension => "Extension",
            Self::Type => "Type",
            Self::Permissions => "Permissions",
            Self::Owner => "Owner",
            Self::RunCount => "Run Count",
            Self::Volume => "Volume",
        }
    }

    /// How the daemon sorts by this column (`None`: not sortable).
    #[must_use]
    pub fn sort_field(self) -> Option<SortField> {
        Some(match self {
            Self::Name => SortField::Name,
            Self::Path => SortField::Path,
            Self::Size => SortField::Size,
            Self::Modified => SortField::Modified,
            Self::Created => SortField::Created,
            Self::Accessed => SortField::Accessed,
            Self::Extension => SortField::Extension,
            Self::Type => SortField::Type,
            Self::RunCount => SortField::RunCount,
            Self::Permissions | Self::Owner | Self::Volume => return None,
        })
    }

    /// The column that sorts by `field`.
    #[must_use]
    pub fn for_sort(field: SortField) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|c| c.sort_field() == Some(field))
    }

    /// Shown unless the user changed it (Everything’s defaults).
    #[must_use]
    pub fn visible_by_default(self) -> bool {
        matches!(self, Self::Name | Self::Path | Self::Size | Self::Modified)
    }

    /// Default width in logical pixels.
    #[must_use]
    pub fn default_width(self) -> u32 {
        match self {
            Self::Name => 320,
            Self::Path => 420,
            Self::Size | Self::Extension | Self::RunCount => 100,
            Self::Permissions => 110,
            Self::Owner | Self::Volume | Self::Type => 140,
            Self::Modified | Self::Created | Self::Accessed => 160,
        }
    }

    /// Numbers read better right-aligned.
    #[must_use]
    pub fn right_aligned(self) -> bool {
        matches!(self, Self::Size | Self::RunCount)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indices_and_sorting_round_trip() {
        for (i, c) in Column::ALL.into_iter().enumerate() {
            assert_eq!(c.index(), i);
            assert_eq!(Column::from_index(i), Some(c));
            if let Some(f) = c.sort_field() {
                assert_eq!(Column::for_sort(f), Some(c));
            }
        }
        assert_eq!(Column::for_sort(SortField::DateRun), None);
        assert_eq!(
            Column::ALL
                .iter()
                .filter(|c| c.visible_by_default())
                .count(),
            4
        );
    }
}
