use ao_core::parity_session_strategy::{
    decide_existing_session_action, ExistingSessionAction, OrchestratorSessionStrategy,
};

#[test]
fn decide_session_action_basic() {
    assert_eq!(
        decide_existing_session_action(OrchestratorSessionStrategy::Reuse, true),
        ExistingSessionAction::ReuseExisting
    );
    assert_eq!(
        decide_existing_session_action(OrchestratorSessionStrategy::Delete, true),
        ExistingSessionAction::DeleteExistingAndReuseName
    );
    assert_eq!(
        decide_existing_session_action(OrchestratorSessionStrategy::Ignore, true),
        ExistingSessionAction::Abort
    );
    assert_eq!(
        decide_existing_session_action(OrchestratorSessionStrategy::Reuse, false),
        ExistingSessionAction::IgnoreExistingAndSpawnNew
    );
}
