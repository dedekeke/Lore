use regex::Regex;
use std::sync::LazyLock;

struct Pattern {
    regex: Regex,
    _name: &'static str,
}

static PATTERNS: LazyLock<Vec<Pattern>> = LazyLock::new(|| {
    vec![
        // PEM blocks (must be before generic patterns to handle multiline)
        Pattern {
            regex: Regex::new(r"-----BEGIN [A-Z ]+-----[\s\S]*?-----END [A-Z ]+-----").unwrap(),
            _name:"pem",
        },
        // JWT tokens
        Pattern {
            regex: Regex::new(r"eyJ[A-Za-z0-9_-]+\.eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+").unwrap(),
            _name:"jwt",
        },
        // AWS access keys
        Pattern {
            regex: Regex::new(r"AKIA[0-9A-Z]{16}").unwrap(),
            _name:"aws",
        },
        // GitHub PATs
        Pattern {
            regex: Regex::new(r"gh[ps]_[A-Za-z0-9_]{36,}").unwrap(),
            _name:"github_pat",
        },
        // Connection strings
        Pattern {
            regex: Regex::new(r#"(?i)(postgres|mysql|mongodb|redis)://[^\s'"]+"#).unwrap(),
            _name:"connection_string",
        },
        // Bearer tokens
        Pattern {
            regex: Regex::new(r"(?i)bearer\s+[A-Za-z0-9._~+/=-]+").unwrap(),
            _name:"bearer",
        },
        // Generic API keys/tokens/secrets/passwords
        Pattern {
            regex: Regex::new(
                r##"(?i)(api[_-]?key|token|secret|password)\s*[:=]\s*['"]?[A-Za-z0-9/+=_-]{16,}['"]?"##,
            )
            .unwrap(),
            _name:"generic_secret",
        },
    ]
});

pub fn scrub(input: &str) -> String {
    let mut result = input.to_string();
    for pattern in PATTERNS.iter() {
        result = pattern
            .regex
            .replace_all(&result, "[REDACTED]")
            .into_owned();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_redact_aws_access_key() {
        let input = "my key is AKIAIOSFODNN7EXAMPLE";
        assert_eq!(scrub(input), "my key is [REDACTED]");
    }

    #[test]
    fn should_redact_generic_api_key() {
        let input = "api_key=abcdefghijklmnop1234";
        assert_eq!(scrub(input), "[REDACTED]");
    }

    #[test]
    fn should_redact_generic_token() {
        let input = r#"token: "sk_live_abcdefghijklmnop""#;
        assert_eq!(scrub(input), "[REDACTED]");
    }

    #[test]
    fn should_redact_generic_secret() {
        let input = "SECRET = ABCDEFGHIJKLMNOPQRSTUV";
        assert_eq!(scrub(input), "[REDACTED]");
    }

    #[test]
    fn should_redact_generic_password() {
        let input = "password=SuperSecretPass1234";
        assert_eq!(scrub(input), "[REDACTED]");
    }

    #[test]
    fn should_redact_pem_block() {
        let input = "before\n-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAK...\nline2\n-----END RSA PRIVATE KEY-----\nafter";
        let result = scrub(input);
        assert_eq!(result, "before\n[REDACTED]\nafter");
        assert!(!result.contains("BEGIN"));
        assert!(!result.contains("MIIEowIBAAK"));
    }

    #[test]
    fn should_redact_connection_string_postgres() {
        let input = "url: postgres://user:pass@host:5432/db";
        assert_eq!(scrub(input), "url: [REDACTED]");
    }

    #[test]
    fn should_redact_connection_string_mysql() {
        let input = "mysql://root:secret@localhost/mydb";
        assert_eq!(scrub(input), "[REDACTED]");
    }

    #[test]
    fn should_redact_connection_string_mongodb() {
        let input = "mongodb://admin:pass@cluster0.example.net/test";
        assert_eq!(scrub(input), "[REDACTED]");
    }

    #[test]
    fn should_redact_connection_string_redis() {
        let input = "redis://default:password@redis-host:6379";
        assert_eq!(scrub(input), "[REDACTED]");
    }

    #[test]
    fn should_redact_jwt_token() {
        let input = "token: eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abc123def456";
        assert_eq!(scrub(input), "token: [REDACTED]");
    }

    #[test]
    fn should_redact_bearer_token() {
        let input = "Authorization: Bearer eyAbcToken123.xyz";
        assert_eq!(scrub(input), "Authorization: [REDACTED]");
    }

    #[test]
    fn should_redact_github_pat() {
        let input = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklm";
        assert_eq!(scrub(input), "[REDACTED]");
    }

    #[test]
    fn should_redact_github_pat_ghs() {
        let input = "token: ghs_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklm";
        assert_eq!(scrub(input), "token: [REDACTED]");
    }

    #[test]
    fn should_pass_clean_text_unchanged() {
        let input = "This is a normal description with no secrets at all.";
        assert_eq!(scrub(input), input);
    }

    #[test]
    fn should_pass_code_without_secrets() {
        let input = "fn main() { println!(\"hello world\"); }";
        assert_eq!(scrub(input), input);
    }

    #[test]
    fn should_not_redact_short_key_values() {
        // Values shorter than 16 chars should not match the generic pattern
        let input = "api_key=short";
        assert_eq!(scrub(input), input);
    }

    #[test]
    fn should_not_redact_partial_aws_key() {
        // Only 12 chars after AKIA instead of 16
        let input = "AKIA12345678ABCD";
        assert_eq!(scrub(input), input);
    }

    #[test]
    fn should_redact_multiple_secrets_in_one_string() {
        let input = "key AKIAIOSFODNN7EXAMPLE and postgres://u:p@h/d";
        let result = scrub(input);
        assert!(!result.contains("AKIA"));
        assert!(!result.contains("postgres://"));
        assert_eq!(result.matches("[REDACTED]").count(), 2);
    }

    #[test]
    fn should_handle_empty_string() {
        assert_eq!(scrub(""), "");
    }

    #[test]
    fn should_redact_pem_with_different_types() {
        let input = "-----BEGIN CERTIFICATE-----\ndata\n-----END CERTIFICATE-----";
        assert_eq!(scrub(input), "[REDACTED]");
    }
}
