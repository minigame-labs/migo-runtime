# Product Vorbis regression inputs

Original synthetic signals from the W09 Ogg corpus, encoded with FFmpeg libvorbis.
The 17/9737-frame inputs expose short EOS and native/converted presentation tails.
Cross is a six-channel 240137-frame stream spanning Ogg pages; max-window-8 is
an eight-channel long-window stream. source.json records independent frame counts,
encoded SHA-256, frozen Linux/x86_64 f32le samples and the full presentation prefix comparison against FFmpeg. FFmpeg raw output can retain
extra tail frames (not a presentation-length oracle).
The golden files are required test source. Never regenerate them in a test gate.
