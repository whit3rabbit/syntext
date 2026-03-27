// User model definitions
// Represents authenticated users of the search API.

use std::fmt;

/// User roles controlling access levels.
#[derive(Debug, Clone, PartialEq)]
pub enum Role {
    Admin,
    Editor,
    Viewer,
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Role::Admin => write!(f, "admin"),
            Role::Editor => write!(f, "editor"),
            Role::Viewer => write!(f, "viewer"),
        }
    }
}

/// A user account in the system.
#[derive(Debug, Clone)]
pub struct User {
    pub id: u64,
    pub name: String,
    pub email: String,
    pub role: Role,
}

impl User {
    pub fn new(id: u64, name: &str, email: &str, role: Role) -> Self {
        User {
            id,
            name: name.to_string(),
            email: email.to_string(),
            role,
        }
    }

    /// Check if the user has admin privileges.
    pub fn is_admin(&self) -> bool {
        self.role == Role::Admin
    }

    /// Check if the user can execute parse_query operations.
    /// Only admins and editors have this permission.
    pub fn can_search(&self) -> bool {
        matches!(self.role, Role::Admin | Role::Editor)
    }

    /// Validate that the email looks reasonable.
    /// Matches pattern: something@domain.tld
    pub fn validate_email(&self) -> bool {
        self.email.contains('@') && self.email.contains('.')
    }
}

/// A batch of users for bulk operations.
pub struct UserBatch {
    pub users: Vec<User>,
}

impl UserBatch {
    /// Process batch of users, filtering to only active ones.
    pub fn process_batch(&self) -> Vec<&User> {
        // TODO: add active flag to User struct
        self.users.iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_admin_can_search() {
        let user = User::new(1, "alice", "alice@example.com", Role::Admin);
        assert!(user.can_search());
    }

    #[test]
    fn test_viewer_cannot_search() {
        let user = User::new(2, "bob", "bob@example.com", Role::Viewer);
        assert!(!user.can_search());
    }
}
