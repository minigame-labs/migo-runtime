import {
  op_media_audio_player_create,
  op_media_audio_player_add_source,
  op_media_audio_player_remove_source,
  op_media_audio_player_start,
  op_media_audio_player_stop,
  op_media_audio_player_destroy,
} from "ext:core/ops";
import { allocateHostCallbackId } from "ext:host_v8_base/02_async.js";

class MediaAudioPlayer {
  #id;
  #destroyed = false;
  #volume = 1.0;
  #sources = new Set();

  constructor() {
    this.#id = allocateHostCallbackId();
    op_media_audio_player_create(this.#id);
  }

  get volume() {
    return this.#volume;
  }

  set volume(value) {
    this.#volume = Math.max(0, Math.min(1, Number(value) || 0));
    // Apply volume to all managed sources
    for (const source of this.#sources) {
      if (source && typeof source.volume !== "undefined") {
        source.volume = this.#volume;
      }
    }
  }

  addAudioSource(source) {
    if (this.#destroyed) return Promise.reject(new Error("MediaAudioPlayer is destroyed"));
    if (source && typeof source._getId === "function") {
      this.#sources.add(source);
      op_media_audio_player_add_source(this.#id, source._getId());
    }
    return Promise.resolve();
  }

  removeAudioSource(source) {
    if (this.#destroyed) return Promise.reject(new Error("MediaAudioPlayer is destroyed"));
    if (source && typeof source._getId === "function") {
      this.#sources.delete(source);
      op_media_audio_player_remove_source(this.#id, source._getId());
    }
    return Promise.resolve();
  }

  start() {
    if (this.#destroyed) return Promise.reject(new Error("MediaAudioPlayer is destroyed"));
    op_media_audio_player_start(this.#id);
    return Promise.resolve();
  }

  stop() {
    if (this.#destroyed) return Promise.reject(new Error("MediaAudioPlayer is destroyed"));
    op_media_audio_player_stop(this.#id);
    return Promise.resolve();
  }

  destroy() {
    if (this.#destroyed) return Promise.resolve();
    this.#destroyed = true;
    this.#sources.clear();
    op_media_audio_player_destroy(this.#id);
    return Promise.resolve();
  }
}

function createMediaAudioPlayer() {
  return new MediaAudioPlayer();
}

export { MediaAudioPlayer, createMediaAudioPlayer };
