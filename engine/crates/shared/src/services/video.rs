//! `createVideo`: native video players shown over (or under) the game.

use crate::protocol::error::ServiceError;

/// Video players: each shown where content put it, played, paused, sought and
/// taken full screen.
///
/// Every method takes the request content made as JSON -- the player's
/// `videoId`, which content takes from the Host id space, the method's options,
/// and `requestId` for a call. A call is answered through the hook
/// [`VIDEO_RESULT_HOOKS`] names for its method number, with `{}` or with
/// `{"error"}`, once what it asked has happened -- `play` once the video plays,
/// `seek` once it is there -- as the reference's promises settle. A command is
/// carried out and answered by nothing.
///
/// What a player does as it plays reaches content through [`VIDEO_EVENT_HOOKS`],
/// each with `{"videoId"}`: one content no longer has -- destroyed, or made by
/// a runtime that was replaced -- names an id content does not hold, and is
/// dropped.
pub trait VideoService: Send + Sync {
    /// Make player `videoId` and show it at `x`, `y`, `width` x `height` in the
    /// game's logical pixels, with the reference's options: `src`, `poster`,
    /// `initialTime`, `playbackRate`, `live`, `objectFit`, `controls`,
    /// `showProgress`, `showProgressInControlMode`, `backgroundColor`,
    /// `autoplay`, `loop`, `muted`, `obeyMuteSwitch`, `enableProgressGesture`,
    /// `enablePlayGesture`, `showCenterPlayBtn`, `underGameView`. `src` and
    /// `poster` are paths the host can open, or URLs. A command.
    fn create(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "createVideo:fail not supported",
        ))
    }

    /// Take player `videoId` away. A command; its calls still unanswered fail.
    fn destroy(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "video.destroy:fail not supported",
        ))
    }

    /// What content changed on player `videoId`: any of `create`'s options. A
    /// command.
    fn set(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported("video:fail not supported"))
    }

    /// Play. A call, answered once it plays.
    fn play(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported("video.play:fail not supported"))
    }

    /// Pause. A call, answered once paused.
    fn pause(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "video.pause:fail not supported",
        ))
    }

    /// Stop, back at the start. A call, answered once stopped.
    fn stop(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported("video.stop:fail not supported"))
    }

    /// Go to `time` seconds. A call, answered once there.
    fn seek(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported("video.seek:fail not supported"))
    }

    /// Fill the screen, turned `direction` (0, 90 counterclockwise, -90
    /// clockwise). A call, answered once full screen.
    fn request_full_screen(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "video.requestFullScreen:fail not supported",
        ))
    }

    /// Leave full screen. A call, answered once back where content put it.
    fn exit_full_screen(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "video.exitFullScreen:fail not supported",
        ))
    }
}

/// The video service's methods by number (`MIGO_VIDEO_*` in
/// `include/migo/host_services.h`).
pub mod video_method {
    pub const CREATE: u32 = 0;
    pub const DESTROY: u32 = 1;
    pub const SET: u32 = 2;
    pub const PLAY: u32 = 3;
    pub const PAUSE: u32 = 4;
    pub const STOP: u32 = 5;
    pub const SEEK: u32 = 6;
    pub const REQUEST_FULL_SCREEN: u32 = 7;
    pub const EXIT_FULL_SCREEN: u32 = 8;
}

/// The hook each video call is answered through, indexed by its method number;
/// a command has none. One table for every SDK.
pub const VIDEO_RESULT_HOOKS: [Option<&str>; 9] = [
    None,
    None,
    None,
    Some("_internalOnVideoPlayResult"),
    Some("_internalOnVideoPauseResult"),
    Some("_internalOnVideoStopResult"),
    Some("_internalOnVideoSeekResult"),
    Some("_internalOnVideoRequestFullScreenResult"),
    Some("_internalOnVideoExitFullScreenResult"),
];

/// The video service's events by number (`MIGO_VIDEO_EVENT_*`).
pub mod video_event {
    pub const PLAY: u32 = 0;
    pub const PAUSE: u32 = 1;
    pub const ENDED: u32 = 2;
    pub const TIME_UPDATE: u32 = 3;
    pub const ERROR: u32 = 4;
    pub const WAITING: u32 = 5;
    pub const PROGRESS: u32 = 6;
}

/// The hook each video event reaches content through, indexed by its event
/// number; each with `{"videoId"}`, TIME_UPDATE with `position` and `duration`
/// (seconds), ERROR with `errMsg`, PROGRESS with `buffered` (percent) and
/// `duration`.
/// Whether a video event is a reading the next one supersedes -- the position,
/// the buffering -- which travels the bounded lane and is dropped when content
/// falls behind, rather than queuing without bound behind a stalled runtime.
/// Every other event must arrive.
pub const fn video_event_droppable(event: u32) -> bool {
    matches!(event, video_event::TIME_UPDATE | video_event::PROGRESS)
}

pub const VIDEO_EVENT_HOOKS: [&str; 7] = [
    "_internalOnVideoPlayEvent",
    "_internalOnVideoPauseEvent",
    "_internalOnVideoEndedEvent",
    "_internalOnVideoTimeUpdateEvent",
    "_internalOnVideoErrorEvent",
    "_internalOnVideoWaitingEvent",
    "_internalOnVideoProgressEvent",
];
