use dellingr::State;

#[test]
fn table_insert_negative_index_counts_from_the_entry_stack() {
    let mut state = State::new();
    state
        .new_table()
        .expect("the test stack has room for a table");
    state
        .push_number(42.0)
        .expect("the test stack has room for the inserted value");

    state
        .table_insert_at(-2, 1)
        .expect("-2 addresses the table below the value on the entry stack");
    assert_eq!(state.get_top(), 1);

    state
        .push_number(1.0)
        .expect("the test stack has room for a table key");
    state
        .get_table_raw(-2)
        .expect("-2 addresses the table below the key");
    assert_eq!(
        state.to_number(-1).expect("the inserted value is numeric"),
        42.0
    );
}
