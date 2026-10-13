//! Enumerations persisted in segments or carried in receipts.
//!
//! Every enum stored in a segment file has a stable one-byte code (`code` /
//! `from_code`). Codes are part of the on-disk format: never renumber an
//! existing variant, only append.

use serde::{Deserialize, Serialize};

/// How much the host trusts a document's origin. Ordered from least to most
/// trusted.
///
/// Trust never turns evidence into instructions: every level is rendered as
/// reference data outside the system role. Hosts can use it for policy, e.g.
/// refusing tool calls in a turn whose evidence includes [`Trust::External`].
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum Trust {
    /// Downloaded pages, tool output, third-party files.
    External,
    /// Past conversation turns (may quote external content).
    Memory,
    /// Files from the user's own workspace or repository.
    #[default]
    Workspace,
    /// Material curated by the host operator.
    Curated,
}

impl Trust {
    pub(crate) fn code(self) -> u8 {
        match self {
            Trust::External => 0,
            Trust::Memory => 1,
            Trust::Workspace => 2,
            Trust::Curated => 3,
        }
    }

    pub(crate) fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => Trust::External,
            1 => Trust::Memory,
            2 => Trust::Workspace,
            3 => Trust::Curated,
            _ => return None,
        })
    }

    /// Lowercase name used in rendered evidence attributes.
    pub fn as_str(self) -> &'static str {
        match self {
            Trust::External => "external",
            Trust::Memory => "memory",
            Trust::Workspace => "workspace",
            Trust::Curated => "curated",
        }
    }
}

/// Where a document came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DocKind {
    /// A file read from a source directory.
    File,
    /// Free-form text supplied by the host.
    #[default]
    Note,
    /// One conversation turn from the memory journal.
    Turn,
    /// Content fetched from outside the workspace.
    External,
}

impl DocKind {
    pub(crate) fn code(self) -> u8 {
        match self {
            DocKind::File => 0,
            DocKind::Note => 1,
            DocKind::Turn => 2,
            DocKind::External => 3,
        }
    }

    pub(crate) fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => DocKind::File,
            1 => DocKind::Note,
            2 => DocKind::Turn,
            3 => DocKind::External,
            _ => return None,
        })
    }
}

/// Structural type of a passage, which drives intent-aware ranking and how a
/// passage may be condensed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PassageKind {
    /// Paragraphs of prose (documentation, notes, comments).
    Prose,
    /// Source code without a recognized named definition.
    Code,
    /// Source code whose primary content is a named definition.
    Definition,
    /// A conversation turn.
    Turn,
    /// Structured data (JSON, YAML, TOML, CSV).
    Data,
}

impl PassageKind {
    pub(crate) fn code(self) -> u8 {
        match self {
            PassageKind::Prose => 0,
            PassageKind::Code => 1,
            PassageKind::Definition => 2,
            PassageKind::Turn => 3,
            PassageKind::Data => 4,
        }
    }

    pub(crate) fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => PassageKind::Prose,
            1 => PassageKind::Code,
            2 => PassageKind::Definition,
            3 => PassageKind::Turn,
            4 => PassageKind::Data,
            _ => return None,
        })
    }

    /// Lowercase name used in rendered evidence attributes.
    pub fn as_str(self) -> &'static str {
        match self {
            PassageKind::Prose => "prose",
            PassageKind::Code => "code",
            PassageKind::Definition => "definition",
            PassageKind::Turn => "turn",
            PassageKind::Data => "data",
        }
    }
}

/// Kind of a named code definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    /// Free function.
    Function,
    /// Function inside a type or impl block.
    Method,
    /// Class.
    Class,
    /// Struct or record.
    Struct,
    /// Enumeration.
    Enum,
    /// Trait, interface, or protocol.
    Interface,
    /// Type alias.
    TypeAlias,
    /// Module or namespace.
    Module,
    /// Constant or static.
    Constant,
    /// Macro.
    Macro,
    /// Implementation block (`impl X`), a container rather than a symbol.
    Impl,
    /// Database object (table, view, procedure).
    Schema,
}

impl SymbolKind {
    pub(crate) fn code(self) -> u8 {
        match self {
            SymbolKind::Function => 1,
            SymbolKind::Method => 2,
            SymbolKind::Class => 3,
            SymbolKind::Struct => 4,
            SymbolKind::Enum => 5,
            SymbolKind::Interface => 6,
            SymbolKind::TypeAlias => 7,
            SymbolKind::Module => 8,
            SymbolKind::Constant => 9,
            SymbolKind::Macro => 10,
            SymbolKind::Impl => 11,
            SymbolKind::Schema => 12,
        }
    }

    pub(crate) fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            1 => SymbolKind::Function,
            2 => SymbolKind::Method,
            3 => SymbolKind::Class,
            4 => SymbolKind::Struct,
            5 => SymbolKind::Enum,
            6 => SymbolKind::Interface,
            7 => SymbolKind::TypeAlias,
            8 => SymbolKind::Module,
            9 => SymbolKind::Constant,
            10 => SymbolKind::Macro,
            11 => SymbolKind::Impl,
            12 => SymbolKind::Schema,
            _ => return None,
        })
    }

    /// Lowercase name, e.g. `"function"`.
    pub fn as_str(self) -> &'static str {
        match self {
            SymbolKind::Function => "function",
            SymbolKind::Method => "method",
            SymbolKind::Class => "class",
            SymbolKind::Struct => "struct",
            SymbolKind::Enum => "enum",
            SymbolKind::Interface => "interface",
            SymbolKind::TypeAlias => "type",
            SymbolKind::Module => "module",
            SymbolKind::Constant => "constant",
            SymbolKind::Macro => "macro",
            SymbolKind::Impl => "impl",
            SymbolKind::Schema => "schema",
        }
    }

    /// True for kinds that group other definitions (types, impls, modules).
    pub fn is_container(self) -> bool {
        matches!(
            self,
            SymbolKind::Class
                | SymbolKind::Struct
                | SymbolKind::Enum
                | SymbolKind::Interface
                | SymbolKind::Module
                | SymbolKind::Impl
        )
    }
}

/// Speaker of a chat message or conversation turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Host or operator instructions.
    System,
    /// The human user.
    User,
    /// The model.
    Assistant,
    /// Tool or function output.
    Tool,
}

impl Role {
    /// Lowercase name as used by OpenAI-compatible chat APIs.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }
}
