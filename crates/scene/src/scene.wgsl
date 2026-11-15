// 窗口的背景：日冕（深色圆盘边缘的光，光丝向外放射、缓缓流动）、绕圆盘弯曲并向外流的网格；面板里以面板底色为主，
// 光从朝圆盘的那条竖边按指数漫进去。坐标是像素，原点在左上。输出已做色调映射与 sRGB 编码，不透明。

struct Params {
    // 宽、高（像素）、每个逻辑点几个像素、光芒在一个循环里走到哪（0 到 1）
    size: vec4f,
    // 圆盘中心 x、y 与半径（像素），网格向外流过的距离（逻辑点，0 到一个格距）
    disc: vec4f,
    // 光的主色（线性的 0 到 1）；w 为日冕与透镜弯曲的强度（0 到 1）
    light: vec4f,
    // 外缘偏向的颜色、亮环的颜色（线性的 0 到 1；w 不用）
    deep: vec4f,
    core: vec4f,
    // 面板外与面板里的底色（sRGB 编码的 0 到 1；w 不用）
    ground: vec4f,
    deck: vec4f,
    // 面板几块、圆角半径（逻辑点）
    panel: vec4f,
    // 面板：左、上、右、下（像素）；只有前 panel.x 块有效
    panels: array<vec4f, 4>,
}

@group(0) @binding(0) var<uniform> params: Params;

const TAU: f32 = 6.2831853;
// 网格的格距（逻辑点）与它在离中心多远处淡到一半
const GRID_STEP: f32 = 46.0;
const GRID_REACH: f32 = 700.0;
// 光漫进面板的深度（逻辑点）
const SPILL: f32 = 85.0;

@vertex
fn vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4f {
    // 一个盖住整个画面的大三角形
    let x = f32((index << 1u) & 2u);
    let y = f32(index & 2u);
    return vec4f(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
}

fn hash(q: vec3f) -> f32 {
    var p = fract(q * 0.3183099 + vec3f(0.11, 0.17, 0.13));
    p = p * 17.0;
    return fract(p.x * p.y * p.z * (p.x + p.y + p.z));
}

fn noise(x: vec3f) -> f32 {
    let i = floor(x);
    var f = fract(x);
    f = f * f * (3.0 - 2.0 * f);
    return mix(
        mix(mix(hash(i), hash(i + vec3f(1.0, 0.0, 0.0)), f.x),
            mix(hash(i + vec3f(0.0, 1.0, 0.0)), hash(i + vec3f(1.0, 1.0, 0.0)), f.x), f.y),
        mix(mix(hash(i + vec3f(0.0, 0.0, 1.0)), hash(i + vec3f(1.0, 0.0, 1.0)), f.x),
            mix(hash(i + vec3f(0.0, 1.0, 1.0)), hash(i + vec3f(1.0, 1.0, 1.0)), f.x), f.y),
        f.z);
}

fn fbm(start: vec3f) -> f32 {
    var p = start;
    var sum = 0.0;
    var amplitude = 0.5;
    for (var i = 0; i < 5; i++) {
        sum += amplitude * noise(p);
        p = p * 2.03 + vec3f(1.7, 9.2, 3.1);
        amplitude *= 0.5;
    }
    return sum;
}

fn aces(x: vec3f) -> vec3f {
    return clamp((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14), vec3f(0.0), vec3f(1.0));
}

// 引力透镜：画面上半径 r 处看到的，是原来半径为返回值处的（正向是 r_in * (1 + k R² / (r_in² + 0.4 R²))，k 为
// 0.9 乘强度；几次迭代求逆）
fn unlens(r: f32, radius: f32, strength: f32) -> f32 {
    let k = 0.9 * strength;
    var inner = r;
    for (var i = 0; i < 6; i++) {
        inner = r / (1.0 + k * radius * radius / (inner * inner + 0.4 * radius * radius));
    }
    return inner;
}

// 点 p 到圆角矩形 rect（左、上、右、下）边上的有符号距离，里面为负
fn rounded(p: vec2f, rect: vec4f, radius: f32) -> f32 {
    let center = (rect.xy + rect.zw) * 0.5;
    let q = abs(p - center) - (rect.zw - rect.xy) * 0.5 + radius;
    return length(max(q, vec2f(0.0))) + min(max(q.x, q.y), 0.0) - radius;
}

// 点 p 离面板 rect 朝圆盘（中心横坐标 toward）的那条竖边多远；圆盘的横坐标落在面板里时取近的那条
fn depth(p: vec2f, rect: vec4f, toward: f32) -> f32 {
    if (toward >= rect.z) {
        return rect.z - p.x;
    }
    if (toward <= rect.x) {
        return p.x - rect.x;
    }
    return min(p.x - rect.x, rect.z - p.x);
}

@fragment
fn fragment(@builtin(position) position: vec4f) -> @location(0) vec4f {
    let p = position.xy;
    let scale = params.size.z;
    let phase = params.size.w;
    let strength = params.light.w;
    let center = params.disc.xy;
    let radius = params.disc.z;
    let uv = p - center;
    let r = length(uv);
    let angle = atan2(uv.y, uv.x);
    let d = (r - radius) / radius;
    let dir = vec2f(cos(angle), sin(angle));
    let around = 0.7 * vec2f(cos(TAU * phase), sin(TAU * phase));

    // 日冕：先扭曲坐标再取噪声，光丝弯曲、成簇
    let warp = fbm(vec3f(dir * 2.2 + around * 0.6, d * 1.2));
    var streak = fbm(vec3f(dir * 9.0 + warp * 1.6, d * 0.9 - around.x * 0.5 + around.y * 0.3));
    streak = pow(clamp(streak * 1.25, 0.0, 1.0), 3.0) * 2.2;
    let fine = fbm(vec3f(dir * 38.0 + warp, d * 3.0 + around.y));
    streak *= 0.65 + 0.7 * fine;
    let outside = max(d, 0.0);
    let corona = exp(-outside / (0.035 + 0.32 * streak)) * (0.18 + 0.8 * streak);
    let halo = exp(-outside / 0.05);
    let glow = exp(-outside / 0.30);
    let ambient = exp(-outside / 1.8);
    // 亮环：红、绿、蓝的边缘各错开一点，带色散的彩边
    let w = 0.008;
    let rim = vec3f(exp(-abs(d - 0.007) / w), exp(-abs(d) / w), exp(-abs(d + 0.006) / w));
    let base = mix(params.light.rgb, params.deep.rgb, clamp(d / 0.9, 0.0, 1.0));
    var hdr = rim * params.core.rgb * 3.2 + halo * base * 1.2 + corona * base * 1.15 + glow * params.deep.rgb * 0.12
        + ambient * mix(params.deep.rgb, params.light.rgb, 0.35) * (0.06 + 0.16 * streak);
    hdr *= smoothstep(-0.01, 0.0, d) * strength;

    // 面板里以面板底色为主：光从朝圆盘的那条竖边漫进去；inside 为点在面板里的覆盖（边上一个像素过渡）
    var fade = 1.0;
    var inside = 0.0;
    let count = i32(params.panel.x);
    let corner = params.panel.y * scale;
    for (var i = 0; i < 4; i++) {
        if (i >= count) {
            break;
        }
        let rect = params.panels[i];
        let cover = clamp(0.5 - rounded(p, rect, corner), 0.0, 1.0);
        if (cover > 0.0) {
            let left = exp(-max(depth(p, rect, center.x), 0.0) / (SPILL * scale));
            fade = mix(1.0, left, cover);
            inside = cover;
        }
    }
    hdr *= fade;
    var color = pow(aces(hdr), vec3f(1.0 / 2.2));

    // 网格：画面上的点先按透镜求回原来的位置，再看离最近的网格线多远；网格线从中心向外流
    let inner = unlens(r, radius, strength) / max(r, 0.001);
    let flat = uv * inner / scale;
    let step = GRID_STEP;
    let flow = params.disc.w;
    let along = abs(flat) - flow;
    let off = abs(fract(along / step + 0.5) - 0.5) * step;
    let width = fwidth(flat) * 0.75;
    let line_x = 1.0 - smoothstep(0.0, max(width.x, 0.001), off.x);
    let line_y = 1.0 - smoothstep(0.0, max(width.y, 0.001), off.y);
    // 离中心越远越淡；刚流出中心的一格淡入
    let reach = exp(-abs(flat) / GRID_REACH) * clamp(abs(flat) / step, vec2f(0.0), vec2f(1.0));
    let grid = max(line_x * reach.x, line_y * reach.y) * 0.11 * fade;
    color = mix(color, params.light.rgb, grid);

    // 胶片颗粒
    color += (hash(vec3f(p, floor(phase * 720.0))) - 0.5) * 0.035;
    let floor = mix(params.ground.rgb, params.deck.rgb, inside);
    return vec4f(max(color, floor), 1.0);
}
