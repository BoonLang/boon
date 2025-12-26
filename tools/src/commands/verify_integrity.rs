//! Integrity verification for Boon playground examples
//!
//! Prevents "shortcuts" where examples are modified instead of fixing the engine.
//! Run this test first to ensure examples haven't been tampered with.

use anyhow::Result;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Expected SHA256 hashes for all 11 main examples
/// Generated from canonical versions in commit qzsxmpzxqwwy
const EXPECTED_HASHES: &[(&str, &str)] = &[
    ("minimal", "d8c852ef3b7e898a07fa923c53e2392ed9db6330b20d7ef7be79c626da2f3ee7"),
    ("hello_world", "a02ce6feade8f4d84fdd179229ef0255cc731066b8911bc29b55ca526435c7f2"),
    ("interval", "7d23d08939f69f1eb81363ee30e965a8de79c1e35bf898d28dc692110a1d76a9"),
    ("interval_hold", "78ae5dc83ad227bb5792c9ef57d8333051d77492e915e4c2dc31d0f376b532e9"),
    ("counter", "59c0124830f65c832ecd34db2f52958ad39aaa36830ba79eb42a2f0067382351"),
    ("counter_hold", "4f78a3767c0e4fdddf26a43d1f10c890e8630d6100735b60efc3fecdfcf4c76b"),
    ("fibonacci", "efc1a761753ee89004b4538d985abf47dd81d515693a5d6c0294e711a7caa1d5"),
    ("layers", "829642462434bb7cec23e2a299ca6a82a73d43bf259a85247ef0853a106b9f6b"),
    ("shopping_list", "185b9c003201d3815e770cf3188a49737fff727775fab64088bd58f725f3b872"),
    ("pages", "3a7998d291daaaba37cb7dd20fbf9fecb7944d913b015258e0f0ab96d8c1ff76"),
    ("todo_mvc", "134cececd1672b9f4fa796e3b7474d605e699b40f5dc1c20d54a872506939b7a"),
];

/// Result of integrity verification
#[derive(Debug)]
pub struct IntegrityResult {
    pub example_name: String,
    pub passed: bool,
    pub expected_hash: String,
    pub actual_hash: Option<String>,
    pub error: Option<String>,
}

/// Verify integrity of all 11 main examples
pub fn verify_example_integrity(examples_dir: Option<&Path>) -> Result<Vec<IntegrityResult>> {
    let examples_dir = if let Some(dir) = examples_dir {
        dir.to_path_buf()
    } else {
        find_examples_dir()?
    };

    let mut results = Vec::new();

    for (name, expected_hash) in EXPECTED_HASHES {
        let path = examples_dir.join(name).join(format!("{}.bn", name));

        let result = match std::fs::read(&path) {
            Ok(content) => {
                let actual_hash = compute_sha256(&content);
                let passed = actual_hash == *expected_hash;

                IntegrityResult {
                    example_name: name.to_string(),
                    passed,
                    expected_hash: expected_hash.to_string(),
                    actual_hash: Some(actual_hash),
                    error: None,
                }
            }
            Err(e) => IntegrityResult {
                example_name: name.to_string(),
                passed: false,
                expected_hash: expected_hash.to_string(),
                actual_hash: None,
                error: Some(format!("Failed to read file {}: {}", path.display(), e)),
            },
        };

        results.push(result);
    }

    Ok(results)
}

/// Run integrity check and print results
pub fn run_integrity_check(examples_dir: Option<PathBuf>) -> Result<bool> {
    println!("Example Integrity Check");
    println!("=======================\n");
    println!("Verifying that no examples have been modified...\n");

    let results = verify_example_integrity(examples_dir.as_deref())?;

    let mut all_passed = true;
    let mut passed_count = 0;
    let mut failed_count = 0;

    for result in &results {
        if result.passed {
            println!("  [PASS] {}", result.example_name);
            passed_count += 1;
        } else {
            println!("  [FAIL] {}", result.example_name);
            if let Some(ref error) = result.error {
                println!("         Error: {}", error);
            } else if let Some(ref actual) = result.actual_hash {
                println!("         Expected: {}", result.expected_hash);
                println!("         Actual:   {}", actual);
                println!();
                println!("         DO NOT modify example files as a shortcut!");
                println!("         Fix the engine instead, or restore with:");
                println!("         jj restore --from qzsxmpzxqwwy playground/frontend/src/examples/{0}/{0}.bn", result.example_name);
            }
            failed_count += 1;
            all_passed = false;
        }
    }

    println!();
    println!("=======================");
    if all_passed {
        println!("{}/{} examples verified", passed_count, results.len());
    } else {
        println!("{} passed, {} FAILED", passed_count, failed_count);
    }

    Ok(all_passed)
}

/// Compute SHA256 hash of content
fn compute_sha256(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    let result = hasher.finalize();
    hex::encode(result)
}

/// Find examples directory relative to cwd
fn find_examples_dir() -> Result<PathBuf> {
    let cwd = std::env::current_dir()?;

    let candidates = [
        cwd.join("../playground/frontend/src/examples"),
        cwd.join("playground/frontend/src/examples"),
        cwd.join("../../playground/frontend/src/examples"),
    ];

    for candidate in &candidates {
        if candidate.exists() {
            return Ok(candidate.canonicalize()?);
        }
    }

    anyhow::bail!(
        "Could not find examples directory. Run from project root or specify --examples-dir"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha256_computation() {
        let content = b"test content";
        let hash = compute_sha256(content);
        assert_eq!(hash.len(), 64); // SHA256 produces 64 hex chars
    }

    #[test]
    fn test_hash_consistency() {
        // Verify the same content always produces the same hash
        let content = b"document: Document/new(root: 123)\n";
        let hash1 = compute_sha256(content);
        let hash2 = compute_sha256(content);
        assert_eq!(hash1, hash2);
    }
}
