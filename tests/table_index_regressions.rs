use dellingr::error::ErrorKind;
use dellingr::{LuaType, State};

#[test]
fn table_get_rejects_the_top_key_index_without_mutating_the_stack() {
    let mut state = State::new();
    state
        .new_table()
        .expect("the test stack has room for a table");

    for index in [1, -1] {
        let error = state
            .get_table(index)
            .expect_err("the table index cannot also be the top key");
        assert!(matches!(
            error.kind,
            ErrorKind::InvalidStackIndex { index: actual } if actual == index
        ));
        assert_eq!(state.get_top(), 1);
        assert_eq!(state.typ(-1), LuaType::Table);

        let error = state
            .get_table_raw(index)
            .expect_err("raw lookup also requires a separate top key");
        assert!(matches!(
            error.kind,
            ErrorKind::InvalidStackIndex { index: actual } if actual == index
        ));
        assert_eq!(state.get_top(), 1);
        assert_eq!(state.typ(-1), LuaType::Table);

        let error = state
            .table_next(index)
            .expect_err("iteration also requires a separate top key");
        assert!(matches!(
            error.kind,
            ErrorKind::InvalidStackIndex { index: actual } if actual == index
        ));
        assert_eq!(state.get_top(), 1);
        assert_eq!(state.typ(-1), LuaType::Table);

        let error = state
            .table_insert_at(index, 1)
            .expect_err("insertion also requires a separate top value");
        assert!(matches!(
            error.kind,
            ErrorKind::InvalidStackIndex { index: actual } if actual == index
        ));
        assert_eq!(state.get_top(), 1);
        assert_eq!(state.typ(-1), LuaType::Table);
    }
}
