package com.migo.runtime.callback;

/**
 * The channel a {@link ShareHandler} settles {@code migo.shareAppMessage()} on.
 * <p>
 * Exactly one of these methods takes effect. The first call settles the
 * request; later calls do nothing, so a share sheet that reports both a send
 * and a dismissal cannot deliver two results to content.
 *
 * <h2>Threading</h2>
 * Every method is safe to call from any thread, which is what you need to show
 * a share sheet and answer on whatever thread its callback arrives on.
 *
 * <p>Calls for a session that has ended are ignored, so you do not have to race
 * teardown to stay correct.
 */
public interface ShareSink {

    /**
     * Report that the share flow finished.
     * <p>
     * The common mini-game platform does not tell content whether the user actually sent the message, and
     * neither does this: {@code shareAppMessage()} resolves when the sheet is
     * done with. Content that pays a reward for sharing is reading a signal the platform
     * never gave it.
     */
    void settleShared();

    /**
     * Report that a recording's share finished, with what came of sharing it to
     * your game center: for {@link ShareHandler#shareGameRecording} only, whose
     * result tells content as {@code shareToGameCenter}. A host without a game
     * center settles with {@link #settleShared()}.
     *
     * @param gameCenterErrCode 0 when the recording reached your game center,
     *                          else your code for why it did not
     * @param gameCenterErrMsg  what content receives as that status's errMsg
     */
    void settleSharedToGameCenter(int gameCenterErrCode, String gameCenterErrMsg);

    /**
     * Settle the request as failed.
     *
     * @param errCode SDK-specific error code, or -1 if there is none
     * @param errMsg  the message content receives verbatim as {@code errMsg},
     *                conventionally {@code "shareAppMessage:fail <reason>"}
     */
    void fail(int errCode, String errMsg);
}
