// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// SQLx 0.8 and 0.9 constrain the binding version below the SQLite WAL
// disclosure fix (RUSTSEC-2026-0459). This crate contains no SQLite code:
// all symbols and native linking come from the patched upstream release.
pub use sqlite_patched::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_sqlite_contains_wal_disclosure_fix() {
        // SAFETY: SQLite's version query takes no pointers and is thread-safe.
        let version = unsafe { sqlite3_libversion_number() };
        assert!(
            version >= 3_054_000,
            "SQLite {version} predates RUSTSEC-2026-0459's fix"
        );
    }

    #[test]
    fn reexported_bindings_execute_prepared_queries() {
        // SAFETY: all handles start null, are initialized by SQLite, used only
        // after checking success, and finalized/closed before returning.
        unsafe {
            let mut db = std::ptr::null_mut();
            assert_eq!(sqlite3_open(c":memory:".as_ptr(), &mut db), SQLITE_OK);
            let mut statement = std::ptr::null_mut();
            let prepared = sqlite3_prepare_v2(
                db,
                c"SELECT ?1 + 1".as_ptr(),
                -1,
                &mut statement,
                std::ptr::null_mut(),
            );
            if prepared != SQLITE_OK {
                sqlite3_close(db);
                panic!("prepare failed: {prepared}");
            }
            let bound = sqlite3_bind_int(statement, 1, 41);
            let stepped = sqlite3_step(statement);
            let value = sqlite3_column_int(statement, 0);
            let finalized = sqlite3_finalize(statement);
            let closed = sqlite3_close(db);
            assert_eq!(bound, SQLITE_OK);
            assert_eq!(stepped, SQLITE_ROW);
            assert_eq!(value, 42);
            assert_eq!(finalized, SQLITE_OK);
            assert_eq!(closed, SQLITE_OK);
        }
    }
}
