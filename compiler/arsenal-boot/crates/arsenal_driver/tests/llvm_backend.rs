//! LLVM-backend run-tests (Phase 13).
//!
//! Each program in this test runs through `arsenal build --backend=llvm`,
//! the resulting executable is run, and its exit code (and stdout when
//! `.expected_stdout` is present) is matched against the corpus's
//! expectation files — same protocol as `phase1_run.rs`.
//!
//! Frontier as of B.3: 168 of the 226 corpus programs. Adding the
//! aggregate ABI (B.4) and string literals + the Print desugar (B.5)
//! closes the remaining 58. Anything not in `SUPPORTED` exercises a
//! MIR construct B.3 doesn't yet handle (`Rvalue::Field`,
//! `MirStmt::AssignField`, `Const::DataAddr`, the implicit Print
//! desugar, or class-/slice-typed signatures).
//!
//! Skipped on Windows for the same reason as `phase1_run.rs`: the
//! driver shells out to `cc`.
//!
//! Build prerequisite: `LLVM_SYS_180_PREFIX=/opt/homebrew/opt/llvm@18`
//! (or the Linux equivalent) must be set when compiling the workspace.

#![cfg(not(windows))]

use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Programs the LLVM backend can compile end-to-end as of B.3.
/// Curated by running `arsenal build --backend=llvm` against every
/// corpus `.gw` and collecting the ones that produce a runnable binary
/// matching its `.expected_exit` (and `.expected_stdout` when present).
const SUPPORTED: &[&str] = &[
    "01_exit_zero",
    "02_arith_add",
    "03_arith_sub",
    "04_arith_mul",
    "05_arith_div",
    "06_arith_mod",
    "07_precedence",
    "08_paren",
    "09_left_assoc",
    "10_bitwise_and",
    "11_shift_left",
    "12_xor",
    "13_simple_call",
    "14_id",
    "15_nested_calls",
    "16_chain_call",
    "17_param_uses",
    "18_let_simple",
    "19_let_two",
    "20_let_with_temps",
    "21_let_chain",
    "22_let_self_use",
    "23_let_in_call",
    "24_if_simple",
    "25_if_else",
    "26_if_cmp",
    "27_else_if",
    "28_while_one_iter",
    "29_while_zero_iter",
    "30_recursive_fact",
    "31_fib",
    "32_max",
    "33_min",
    "34_abs",
    "35_eq_int",
    "36_neq_int",
    "37_log_and",
    "38_log_or",
    "39_log_not",
    "40_bool_let",
    "41_bool_eq",
    "42_chained_cmp",
    "43_lte",
    "44_gte",
    "45_falls_through",
    "46_assign_basic",
    "47_assign_chain",
    "48_assign_param",
    "49_while_count",
    "50_while_sum",
    "51_break_in_while",
    "52_continue_skips",
    "53_for_sum",
    "54_for_inclusive",
    "55_for_break",
    "56_for_continue",
    "57_nested_for",
    "58_iter_fact",
    "59_sum_10",
    "60_putchar_hi",
    "61_putchar_loop",
    "62_putchar_for",
    "63_extern_round_trip",
    "72_top_level_stmts_basic",
    "73_top_level_implicit_return_zero",
    "74_top_level_if_controls_exit",
    "75_top_level_calls_user_fn",
    "76_top_level_for_loop",
    // B.3 surface — float arithmetic / comparison / control flow:
    "89_float_basic_add",
    "90_float_sub",
    "91_float_mul",
    "92_float_div",
    "93_float_neg",
    "94_float_cmp_lt",
    "95_float_cmp_le",
    "96_float_cmp_gt",
    "97_float_cmp_ge",
    "98_float_cmp_ne",
    "99_float_paren_precedence",
    "100_float_chained_arith",
    "101_float_param",
    "102_float_loop_accumulate",
    "103_float_recursion",
    "104_and_short_skips_rhs",
    "105_or_short_skips_rhs",
    "106_and_evaluates_rhs",
    "107_or_evaluates_rhs",
    "108_and_chain_short",
    "109_or_chain_short",
    "110_mixed_and_or_precedence",
    "111_short_circuit_in_while",
    "112_nested_short_circuit",
    "113_short_circuit_with_let",
    "114_short_circuit_runtime_lhs",
    "115_popcount",
    "116_parity",
    "117_byte_pack",
    "118_byte_extract",
    "119_nibble_split",
    "120_mask_set_clear_toggle",
    "121_is_power_of_two",
    "122_round_up_pow2",
    "124_sign_extract",
    "125_abs_branchless",
    "126_reverse_bits_8",
    "127_fib_iter_i32",
    "128_fact_iter_i32",
    "129_fact_iter_i64",
    "130_fact_recur_i64",
    "131_fib_recur_i64",
    "132_fib_iter_i64",
    "133_gcd_iter_i64",
    "134_gcd_recur_u64",
    "135_ackermann_i32",
    "136_collatz_steps_i32",
    "137_collatz_max_i64",
    "138_int_sqrt_i32",
    "139_primality_i32",
    "140_power_iter_i32",
    "141_power_recur_i64",
    "142_fib_u64",
    "156_print_alphabet_via_putchar",
    "157_print_decimal_digit",
    "158_print_int_recursive",
    "163_print_padding",
    "164_print_table_rows",
    "167_abs_basic",
    "168_abs_chain_putchar",
    "169_getpid_positive",
    "170_getpid_consistent",
    "171_abs_in_loop_bound",
    "172_abs_runtime_input",
    "174_extern_chain_arith",
    "175_multi_extern_decls",
    "176_extern_in_short_circuit",
    "177_signed_arith_shift",
    "178_signed_div_negative",
    "179_unsigned_div",
    "181_i64_in_condition",
    "182_u64_bitwise",
    "183_i64_negative_arith",
    "184_i64_shift",
    // B.3 surface — float corner cases:
    "185_float_neg_zero",
    "186_float_div_by_int_pattern",
    "187_float_compare_zero",
    "188_float_loop_sum",
    "189_for_inclusive",
    "190_nested_for",
    "191_break_continue_nested",
    "192_deep_recursion",
    "196_bitwise_not",
    "197_paren_depth",
    "198_chained_compare_via_and",
    "199_return_from_deep",
    // B.3 surface — `as` cast matrix (incl. float→int saturation):
    "201_cast_widen_signed",
    "202_cast_widen_unsigned",
    "203_cast_narrow_truncates",
    "204_cast_signedness_reinterpret",
    "205_cast_widens_for_overflow_safe_mul",
    "206_cast_at_call_site",
    "207_cast_chain_through_widths",
    "208_cast_negated_literal",
    "209_cast_int_to_float",
    "210_cast_uint_to_float",
    "211_cast_float_to_int_truncates",
    "212_cast_float_widths",
    "213_cast_float_in_arith",
    "214_cast_float_saturates",
];

fn corpus_dir() -> PathBuf {
    let manifest = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest)
        .join("..")
        .join("..")
        .join("..")
        .join("..")
        .join("tests")
        .join("snake_eater")
        .join("pass")
        .join("phase1")
        .canonicalize()
        .expect("canonicalize phase1 corpus path")
}

fn arsenal_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_arsenal"))
}

fn unique_tmp(name: &str) -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut p = std::env::temp_dir();
    p.push(format!("arsenal-llvm-{name}-{pid}-{nanos}"));
    fs::create_dir(&p).expect("create tempdir");
    p
}

#[test]
fn llvm_backend_compiles_and_runs_supported_programs() {
    let dir = corpus_dir();
    let arsenal = arsenal_binary();

    for stem in SUPPORTED {
        let src = dir.join(format!("{stem}.gw"));
        assert!(src.is_file(), "missing corpus source {}", src.display());
        let exit_path = src.with_extension("expected_exit");
        let expected_exit: i32 = fs::read_to_string(&exit_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", exit_path.display()))
            .trim()
            .parse()
            .unwrap_or_else(|e| panic!("parse {}: {e}", exit_path.display()));

        let tmp = unique_tmp(stem);
        let staged = tmp.join(format!("{stem}.gw"));
        fs::copy(&src, &staged).expect("copy source");

        let build_args: Vec<OsString> = vec![
            "build".into(),
            "--backend=llvm".into(),
            staged.as_os_str().to_owned(),
        ];
        let build = Command::new(&arsenal)
            .args(&build_args)
            .status()
            .expect("invoke arsenal build --backend=llvm");
        assert!(
            build.success(),
            "`arsenal build --backend=llvm {}` failed (status {build:?})",
            staged.display()
        );

        let exe = tmp.join(stem);
        let run = Command::new(&exe)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .output()
            .unwrap_or_else(|e| panic!("invoke {}: {e}", exe.display()));
        let actual_exit = run
            .status
            .code()
            .expect("process exited via signal, not exit code");
        assert_eq!(
            actual_exit, expected_exit,
            "{stem}: expected exit {expected_exit}, got {actual_exit}"
        );

        let expected_stdout_path = src.with_extension("expected_stdout");
        if expected_stdout_path.is_file() {
            let expected = fs::read(&expected_stdout_path)
                .unwrap_or_else(|e| panic!("read {}: {e}", expected_stdout_path.display()));
            assert_eq!(
                run.stdout,
                expected,
                "{stem}: stdout mismatch\n  expected: {:?}\n  actual:   {:?}",
                String::from_utf8_lossy(&expected),
                String::from_utf8_lossy(&run.stdout),
            );
        }

        let _ = fs::remove_dir_all(&tmp);
    }
}
