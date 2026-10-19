// direct
wx.createCanvas();
wx.getSystemInfoSync();
wx.thisApiDoesNotExistAnywhere();   // not a wx API either
wx.createOffScreenCanvas();          // wx has it, this build does not
// bracket literal
wx["getLaunchOptionsSync"]();
// destructuring
const { onTouchStart, offTouchStart } = wx;
// alias -- the case a naive scanner misses
const W = wx;
W.createInnerAudioContext();
W.reportMonitor();                   // wx has it
// migo-only
migo.getGamepads();

// The engine, as a "Separate Engine" build loads it.
requirePlugin("cocos");
