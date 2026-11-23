use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Default)]
struct CountingSecretStore {
    reads: Arc<AtomicUsize>,
}

impl datatab_core::SecretStore for CountingSecretStore {
    fn is_available(&self) -> bool {
        true
    }
    fn get(&self, _: &str) -> Result<Option<SecretString>, datatab_core::DbError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    }
    fn set(&self, _: &str, _: &SecretString) -> Result<(), datatab_core::DbError> {
        Ok(())
    }
    fn delete(&self, _: &str) -> Result<(), datatab_core::DbError> {
        Ok(())
    }
}

fn connect_with_value_ref(field: &str, cx: &mut TestAppContext) -> usize {
    let (fixture, cx) = password_fixture(true, cx);
    let store = CountingSecretStore::default();
    fixture.state.update(cx, |state, _| {
        state.profiles_mut()[0].value_refs.insert(
            field.into(),
            datatab_core::ValueRef::env("DATATAB_TEST_VALUE"),
        );
        *state.secret_store().write().expect("store") = Box::new(store.clone());
    });
    fixture.sidebar.update(cx, |sidebar, cx| {
        sidebar.connect_via_pipeline(fixture.profile_id, None, cx)
    });
    cx.run_until_parked();
    store.reads.load(Ordering::SeqCst)
}

#[gpui::test]
fn pipeline_connect_reads_no_keyring_when_a_value_ref_supplies_the_password(
    cx: &mut TestAppContext,
) {
    assert_eq!(connect_with_value_ref("password", cx), 0);
}

#[gpui::test]
fn pipeline_connect_still_reads_the_keyring_when_the_password_lives_there(cx: &mut TestAppContext) {
    assert_eq!(connect_with_value_ref("host", cx), 1);
}
