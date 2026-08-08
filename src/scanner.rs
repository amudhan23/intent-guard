pub fn scan_body(body: &str, patterns: &[String]) -> Option<String> {
    for pattern in patterns {
        if body.contains(pattern) {
            return Some(pattern.clone());
        }
    }
    None
}

pub fn scan_privilege(method: &str, path: &str, risky_paths: &[String]) -> bool {
    method == "PUT" && risky_paths.iter().any(|p| path.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_sql_injection() {
        let patterns = vec!["DROP TABLE".to_string(), "; --".to_string()];
        let result = scan_body("'; DROP TABLE users; --", &patterns);
        assert!(result.is_some());
    }

    #[test]
    fn clean_body_not_flagged() {
        let patterns = vec!["DROP TABLE".to_string(), "; --".to_string()];
        let result = scan_body("just a normal comment, nothing weird", &patterns);
        assert!(result.is_none());
    }

    #[test]
    fn detects_privilege_escalation() {
        let paths = vec!["/admin/".to_string()];
        let result = scan_privilege("PUT", "/admin/users/role", &paths);
        assert!(result);
    }

    #[test]
    fn get_request_not_flagged_as_privilege() {
        let paths = vec!["/admin/".to_string()];
        let result = scan_privilege("GET", "/admin/users/role", &paths);
        assert!(!result);
    }
}
