//! What content is shown with: one rule, for both executions.
//!
//! A show carries the options the host enters the game with -- `onShow`'s and `getEnterOptionsSync()`'s -- or none.
//! One that carries none shows the game as it was last entered, starting from the options the session was launched
//! with ([`InitOptions::launch_options`]): the first show of a host that never sends options, which is every C ABI
//! host, is the launch the host described, and a later one does not put defaults no host gave in place of what the
//! game was entered with. A restart launches the game again with the last options, its query the one the restart
//! names.
//!
//! The embedded host and the external-frame session each kept this, and the external one did not fall back: a show
//! without options there reset the enter options to the defaults.
//!
//! [`InitOptions::launch_options`]: shared::config::InitOptions::launch_options

use std::borrow::Cow;

use serde_json::{Map, Value};
use shared::js_escape::{HOOK_ARGS_NONE, hook_args_one};
use tracing::warn;

/// The options content was last shown with, or `None` while there are none -- a session launched without options
/// and never shown with any.
#[derive(Debug, Default)]
pub(crate) struct ShowOptions {
    last: Option<Value>,
}

impl ShowOptions {
    /// Before the first show: the options the session was launched with.
    pub(crate) fn launched_with(launch_options: Option<&Map<String, Value>>) -> Self {
        Self {
            last: launch_options.cloned().map(Value::Object),
        }
    }

    /// The runtime is launched: its `_internalLaunch` arguments, the options it was last shown with -- or launched
    /// with, before any show -- or `None` while there are none, when it is launched with its first show's.
    #[cfg(feature = "embedded-v8")]
    pub(crate) fn launch(&self) -> Option<Cow<'static, str>> {
        self.last.as_ref().map(hook_args_one)
    }

    /// The host shows the game with `options_json`: its `_internalTriggerOnShow` arguments. Options that are not a
    /// JSON object are none, as an absent one is.
    pub(crate) fn show(&mut self, options_json: Option<&str>) -> Cow<'static, str> {
        if let Some(json) = options_json.map(str::trim).filter(|json| !json.is_empty()) {
            match serde_json::from_str::<Value>(json) {
                Ok(options) if options.is_object() => self.last = Some(options),
                _ => warn!(
                    "onShow options the host sent are not a JSON object; shown with the last ones"
                ),
            }
        }
        self.args()
    }

    /// The game is launched again: the arguments of the show that relaunch ends with -- the last options, with
    /// `launch_query` as their query when the restart names one.
    #[cfg(feature = "embedded-v8")]
    pub(crate) fn relaunch(&mut self, launch_query: Option<String>) -> Cow<'static, str> {
        self.relaunched(launch_query);
        self.args()
    }

    /// The game is launched again in a new session: the options it is launched with there, as the JSON object a
    /// host creates a session with.
    #[cfg(feature = "external-frames")]
    pub(crate) fn relaunch_json(&mut self, launch_query: Option<String>) -> String {
        self.relaunched(launch_query).to_string()
    }

    fn relaunched(&mut self, launch_query: Option<String>) -> &Value {
        let mut options = self
            .last
            .take()
            .unwrap_or_else(|| Value::Object(Map::new()));
        if let (Some(query), Some(object)) = (launch_query, options.as_object_mut()) {
            object.insert("query".to_owned(), Value::String(query));
        }
        self.last.insert(options)
    }

    fn args(&self) -> Cow<'static, str> {
        self.last
            .as_ref()
            .map_or(Cow::Borrowed(HOOK_ARGS_NONE), hook_args_one)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn launched(options: Value) -> ShowOptions {
        let Value::Object(options) = options else {
            panic!("launch options are an object");
        };
        ShowOptions::launched_with(Some(&options))
    }

    /// The arguments as the dispatcher reads them.
    fn args(encoded: Cow<'static, str>) -> Value {
        serde_json::from_str(&encoded).expect("hook arguments are JSON")
    }

    /// The first show of a host that sends no options is the launch the session was given; a show that sends some
    /// enters with them, and one after it that sends none keeps them rather than the launch's or the defaults.
    #[test]
    fn a_show_without_options_is_the_launch_and_then_the_last_entry() {
        let mut shows = launched(json!({"scene": 1007, "query": {"from": "share"}}));
        assert_eq!(
            args(shows.show(None)),
            json!([{"scene": 1007, "query": {"from": "share"}}])
        );
        assert_eq!(
            args(shows.show(Some(r#"{"scene":1038}"#))),
            json!([{"scene": 1038}])
        );
        assert_eq!(args(shows.show(None)), json!([{"scene": 1038}]));
        assert_eq!(args(shows.show(Some("  "))), json!([{"scene": 1038}]));
        // What a show carried that is not an object is kept as nothing.
        assert_eq!(args(shows.show(Some("[1]"))), json!([{"scene": 1038}]));
        assert_eq!(args(shows.show(Some("not json"))), json!([{"scene": 1038}]));
    }

    /// Launched without options and shown without any, content is told nothing, and the engine's defaults apply.
    #[test]
    fn a_session_launched_without_options_shows_with_none() {
        let mut shows = ShowOptions::launched_with(None);
        assert_eq!(shows.show(None), HOOK_ARGS_NONE);
    }

    /// The runtime is launched with the host's options before content runs; with none it is not, and its first show
    /// launches it. A runtime started after a show -- a restart -- is launched with what the game was last shown with.
    #[cfg(feature = "embedded-v8")]
    #[test]
    fn a_runtime_is_launched_with_the_host_s_options_or_the_last_show_s() {
        let launched = launched(json!({"scene": 1007}));
        assert_eq!(launched.launch().map(args), Some(json!([{"scene": 1007}])));
        let mut shows = ShowOptions::launched_with(None);
        assert_eq!(shows.launch(), None);
        shows.show(Some(r#"{"scene":1038}"#));
        assert_eq!(shows.launch().map(args), Some(json!([{"scene": 1038}])));
    }

    /// A restart launches with the options the game was last shown with, its query the one the restart names; with
    /// none named the query is kept, and a game never shown is launched with nothing but the named query.
    #[cfg(feature = "embedded-v8")]
    #[test]
    fn a_restart_launches_with_the_last_options_and_the_query_it_names() {
        let mut shows = ShowOptions::launched_with(None);
        shows.show(Some(
            r#"{"scene":1011,"query":{"from":"share"},"shareTicket":"t"}"#,
        ));
        let restarted = json!([{"scene": 1011, "query": "level=2&mode=hard", "shareTicket": "t"}]);
        assert_eq!(
            args(shows.relaunch(Some("level=2&mode=hard".into()))),
            restarted
        );
        assert_eq!(args(shows.relaunch(None)), restarted);
        // The relaunch is what the game was last entered with from then on.
        assert_eq!(args(shows.show(None)), restarted);
        assert_eq!(
            args(ShowOptions::launched_with(None).relaunch(Some("a=1".into()))),
            json!([{"query": "a=1"}])
        );
        assert_eq!(
            args(ShowOptions::launched_with(None).relaunch(None)),
            json!([{}])
        );
        assert_eq!(
            args(launched(json!({"scene": 1007})).relaunch(Some("a=1".into()))),
            json!([{"scene": 1007, "query": "a=1"}])
        );
    }

    /// A relaunch in a new session is launched with the same options, as the object a session is created with.
    #[cfg(feature = "external-frames")]
    #[test]
    fn a_relaunch_in_a_new_session_carries_the_options_as_a_session_s() {
        let mut shows = launched(json!({"scene": 1007, "query": {"from": "share"}}));
        let json = shows.relaunch_json(Some("level=2".into()));
        assert_eq!(
            serde_json::from_str::<Value>(&json).unwrap(),
            json!({"scene": 1007, "query": "level=2"})
        );
        assert_eq!(
            serde_json::from_str::<Value>(&ShowOptions::launched_with(None).relaunch_json(None))
                .unwrap(),
            json!({})
        );
    }
}
