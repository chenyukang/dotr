const canvas = document.querySelector("#packet-field");
const ctx = canvas.getContext("2d");

const sources = [
  "~/.zshrc",
  "~/.gitconfig",
  "~/.ssh/config",
  "~/.config/nvim",
  "~/.config/zed",
  "~/.hermes/skills",
];

const targets = [
  "files/home/.zshrc",
  "files/home/.gitconfig",
  "files/home/.ssh/config",
  "files/home/.config/nvim",
  "files/home/.config/zed",
  "metadata/index.json",
];

let width = 0;
let height = 0;
let streams = [];

function resize() {
  const ratio = window.devicePixelRatio || 1;
  width = canvas.clientWidth;
  height = canvas.clientHeight;
  canvas.width = Math.floor(width * ratio);
  canvas.height = Math.floor(height * ratio);
  ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
  seedStreams();
}

function layout() {
  const compact = width < 680;
  const leftX = compact ? 30 : Math.max(52, width * 0.08);
  const rightX = compact ? width - 210 : width - Math.max(420, width * 0.32);
  const top = compact ? Math.max(156, height * 0.2) : Math.max(112, height * 0.18);
  const rowGap = compact ? 38 : 48;
  return { compact, leftX, rightX, top, rowGap };
}

function sourcePoint(index) {
  const { leftX, top, rowGap } = layout();
  return { x: leftX, y: top + index * rowGap };
}

function targetPoint(index) {
  const { rightX, top, rowGap } = layout();
  return { x: rightX, y: top + index * rowGap };
}

function repositoryBounds() {
  const { rightX, top, rowGap, compact } = layout();
  return {
    x: rightX - 18,
    y: top - 42,
    width: compact ? 190 : 350,
    height: rowGap * targets.length + 34,
  };
}

function seedStreams() {
  streams = sources.map((_, index) => ({
    index,
    progress: Math.random(),
    speed: 0.0016 + Math.random() * 0.0012,
    pulse: Math.random() * Math.PI * 2,
  }));
}

function drawGrid(time) {
  const gap = 32;
  const offset = (time * 0.01) % gap;
  ctx.strokeStyle = "rgba(69, 217, 255, 0.075)";
  ctx.lineWidth = 1;

  for (let x = -gap + offset; x <= width + gap; x += gap) {
    ctx.beginPath();
    ctx.moveTo(x, 0);
    ctx.lineTo(x, height);
    ctx.stroke();
  }

  for (let y = -gap + offset; y <= height + gap; y += gap) {
    ctx.beginPath();
    ctx.moveTo(0, y);
    ctx.lineTo(width, y);
    ctx.stroke();
  }
}

function drawColumnTitle(text, x, y, color) {
  ctx.font = "700 12px ui-monospace, SFMono-Regular, Menlo, monospace";
  ctx.fillStyle = color;
  ctx.fillText(text, x, y);
}

function drawPathLabel(text, x, y) {
  ctx.font = "12px ui-monospace, SFMono-Regular, Menlo, monospace";
  ctx.fillStyle = "rgba(237, 244, 255, 0.56)";
  ctx.fillText(text, x, y);
}

function drawNode(x, y, color) {
  ctx.fillStyle = color;
  ctx.beginPath();
  ctx.arc(x, y, 4, 0, Math.PI * 2);
  ctx.fill();
}

function drawFlow(stream, time) {
  const from = sourcePoint(stream.index);
  const to = targetPoint(stream.index);
  const midX = (from.x + to.x) / 2;
  const wobble = Math.sin(time * 0.002 + stream.pulse) * 18;

  ctx.strokeStyle = "rgba(69, 217, 255, 0.18)";
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.moveTo(from.x, from.y);
  ctx.bezierCurveTo(midX, from.y + wobble, midX, to.y - wobble, to.x, to.y);
  ctx.stroke();

  stream.progress += stream.speed;
  if (stream.progress > 1) stream.progress = 0;

  const t = stream.progress;
  const x = cubic(from.x, midX, midX, to.x, t);
  const y = cubic(from.y, from.y + wobble, to.y - wobble, to.y, t);
  const color = stream.index === sources.length - 1 ? "#f2c86b" : "#57ff9a";
  drawNode(x, y, color);
}

function cubic(a, b, c, d, t) {
  const one = 1 - t;
  return one ** 3 * a + 3 * one ** 2 * t * b + 3 * one * t ** 2 * c + t ** 3 * d;
}

function drawRepositoryShape() {
  const box = repositoryBounds();

  ctx.strokeStyle = "rgba(87, 255, 154, 0.28)";
  ctx.fillStyle = "rgba(16, 20, 27, 0.34)";
  ctx.lineWidth = 1;
  ctx.fillRect(box.x, box.y, box.width, box.height);
  ctx.strokeRect(box.x, box.y, box.width, box.height);
}

function drawFileMap() {
  const { leftX, rightX, top, rowGap, compact } = layout();
  const leftLabelX = leftX;
  const rightLabelX = rightX + 16;

  if (!compact) {
    drawColumnTitle("$HOME sources", leftX, top - 28, "#f2c86b");
  }
  drawRepositoryShape();
  if (!compact) {
    const box = repositoryBounds();
    drawColumnTitle("git backup repo", box.x + 18, box.y + 22, "#57ff9a");
  }

  sources.forEach((source, index) => {
    const from = sourcePoint(index);
    const to = targetPoint(index);
    drawNode(from.x, from.y, "#45d9ff");
    drawNode(to.x, to.y, index === targets.length - 1 ? "#f2c86b" : "#57ff9a");
    if (!compact) {
      drawPathLabel(source, leftLabelX + 14, from.y + 4);
      drawPathLabel(targets[index], rightLabelX, to.y + 4);
    }
  });

  if (!compact) {
    ctx.font = "11px ui-monospace, SFMono-Regular, Menlo, monospace";
    ctx.fillStyle = "rgba(255, 94, 168, 0.48)";
    ctx.fillText("sha256 + mode + mtime", rightX + 16, top + rowGap * targets.length + 34);
  }
}

function frame(time) {
  ctx.clearRect(0, 0, width, height);
  ctx.fillStyle = "#08090d";
  ctx.fillRect(0, 0, width, height);
  drawGrid(time);
  drawFileMap();
  streams.forEach((stream) => drawFlow(stream, time));
  requestAnimationFrame(frame);
}

window.addEventListener("resize", resize);
resize();
requestAnimationFrame(frame);
