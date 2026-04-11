use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;

use rewind_store::Store;
use crate::StoreEvent;

pub async fn start_polling(
    store: Arc<Mutex<Store>>,
    event_tx: broadcast::Sender<StoreEvent>,
    poll_interval: Duration,
) {
    let mut known_sessions: std::collections::HashMap<String, u32> = std::collections::HashMap::new();

    // Seed initial state
    if let Ok(guard) = store.lock()
        && let Ok(sessions) = guard.list_sessions() {
            for s in &sessions {
                known_sessions.insert(s.id.clone(), s.total_steps);
            }
        }

    loop {
        tokio::time::sleep(poll_interval).await;

        let updates = {
            let guard = match store.lock() {
                Ok(g) => g,
                Err(_) => continue,
            };
            let sessions = match guard.list_sessions() {
                Ok(s) => s,
                Err(_) => continue,
            };

            let mut new_events = Vec::new();
            for session in &sessions {
                let prev_steps = known_sessions.get(&session.id).copied().unwrap_or(0);
                if session.total_steps > prev_steps {
                    // Find the root timeline
                    if let Ok(Some(timeline)) = guard.get_root_timeline(&session.id)
                        && let Ok(steps) = guard.get_steps(&timeline.id) {
                            for step in steps {
                                if step.step_number > prev_steps {
                                    new_events.push(StoreEvent::StepCreated {
                                        session_id: session.id.clone(),
                                        step: Box::new(step),
                                    });
                                }
                            }
                        }

                    new_events.push(StoreEvent::SessionUpdated {
                        session_id: session.id.clone(),
                        status: session.status.as_str().to_string(),
                        total_steps: session.total_steps,
                        total_tokens: session.total_tokens,
                    });

                    known_sessions.insert(session.id.clone(), session.total_steps);
                } else if !known_sessions.contains_key(&session.id) {
                    known_sessions.insert(session.id.clone(), session.total_steps);
                }
            }
            new_events
        };

        for event in updates {
            let _ = event_tx.send(event);
        }
    }
}
