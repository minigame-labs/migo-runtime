// Draws once, at the top level, and never asks for a frame: no requestAnimationFrame, nothing
// after. scripts/test-player-draws-outside-raf.sh requires the screen to show it.
const canvas = migo.createCanvas();
const context = canvas.getContext("2d");
context.fillStyle = "#c03080";
context.fillRect(0, 0, canvas.width, canvas.height);
