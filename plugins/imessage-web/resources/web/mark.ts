/**
 * The Agent's mark while a turn runs: the logo as an upright slab of iridescent metal that turns once
 * and holds (1.2 s, then 0.8 s), raymarched with WebGL into one offscreen canvas and copied into each
 * running turn's author line, bare, without the icon's ink tile. The portal is plain HTTP, which WebGL accepts (WebGPU would not). Where
 * WebGL is missing, {@link spinningMark} returns null and the page keeps the static mark.
 */

const SPIN = 1.2;
const HOLD = 0.8;
/** The spinning mark fills the author line's 16 CSS px. */
const SIZE = 16;

const VERT = "attribute vec2 p;void main(){gl_Position=vec4(p,0.,1.);}";
// The mark is an L-shaped slab (arms 1 long, 0.33 wide, 0.136 thick: the logo's proportions) turned
// 45 degrees to read as ">". Its faces are slightly domed metal under a studio of soft lights, tinted
// by a thin film whose colour shifts with the viewing angle; ink lines its edges.
const FRAG = `precision highp float;
uniform vec2 uRes;
uniform float uAngle;
const float W=.33,T=.136,R=.024;
const vec3 LIME=vec3(.604,1.,0.),INK=vec3(.0027,.0027,.0037);
float gc,gs;
float box2(vec2 p,vec2 b){vec2 d=abs(p)-b;return length(max(d,0.))+min(max(d.x,d.y),0.);}
vec3 toLocal(vec3 p){p.xz=mat2(gc,-gs,gs,gc)*p.xz;return p;}
vec3 toWorld(vec3 p){p.xz=mat2(gc,gs,-gs,gc)*p.xz;return p;}
float mark(vec3 p){
  p=toLocal(p);
  vec2 q=vec2(p.x-.4703,p.y);
  vec2 l=vec2(q.x-q.y,q.x+q.y)*.70710678;
  float a=box2(l-vec2(-.5,-W*.5),vec2(.5,W*.5)-R);
  float b=box2(l-vec2(-W*.5,-.5),vec2(W*.5,.5)-R);
  vec2 w=vec2(min(a,b),abs(p.z)-(T*.5-R));
  return min(max(w.x,w.y),0.)+length(max(w,0.))-R;
}
vec3 normalAt(vec3 p){
  const vec2 k=vec2(1.,-1.);const float h=.0006;
  return normalize(k.xyy*mark(p+k.xyy*h)+k.yyx*mark(p+k.yyx*h)+k.yxy*mark(p+k.yxy*h)+k.xxx*mark(p+k.xxx*h));
}
float march(vec3 ro,vec3 rd){
  float t=0.;
  for(int i=0;i<96;i++){float d=mark(ro+rd*t);if(d<.0003*(1.+t))return t;t+=d*.92;if(t>14.)break;}
  return -1.;
}
vec3 studio(vec3 r){
  vec3 c=mix(vec3(.015),vec3(.08),smoothstep(-.3,.9,r.y));
  c+=vec3(.95)*smoothstep(.55,.97,dot(r,normalize(vec3(-.3,.35,1.))));
  vec2 h=normalize(r.xz+vec2(.0001));
  float tall=smoothstep(-.25,.25,r.y)*(1.-smoothstep(.7,.95,r.y));
  c+=vec3(2.4)*smoothstep(.93,.995,dot(h,normalize(vec2(-.85,.5))))*tall;
  c+=vec3(1.5)*smoothstep(.94,.997,dot(h,normalize(vec2(.9,-.2))))*tall;
  c+=vec3(1.7)*smoothstep(.93,.995,dot(h,normalize(vec2(.25,-1.))))*tall;
  c+=vec3(1.4)*smoothstep(.93,.995,dot(h,normalize(vec2(-.7,-.7))))*tall;
  c+=vec3(.22)*exp(-r.y*r.y*18.);
  c+=vec3(.8)*smoothstep(.75,.98,r.y);
  return c;
}
vec3 film(float cosI,float d){
  float cosT=sqrt(max(1.-(1.-cosI*cosI)/2.1025,0.));
  vec3 ph=6.2831853*2.9*d*cosT/vec3(650.,532.,450.);
  return pow(.5+.5*cos(ph),vec3(2.4))*1.35;
}
vec3 shade(vec3 p,vec3 rd){
  vec3 n=normalAt(p);
  vec3 nl=n;nl.xz=mat2(gc,-gs,gs,gc)*n.xz;
  float face=abs(nl.z);
  float faceMask=smoothstep(.9,.98,face);
  float edge=smoothstep(.08,.25,face)*(1.-smoothstep(.8,.95,face));
  vec3 lp=toLocal(p);
  n=normalize(n+toWorld(vec3(lp.xy,0.)*.14*faceMask));
  float cosI=max(dot(n,-rd),0.);
  vec3 r=reflect(rd,n);
  float d=mix(480.,820.,.5+.25*sin(lp.x*2.3+lp.y*1.7)+.25*sin(lp.y*3.1-lp.x*1.2+1.3));
  vec3 F0=mix(LIME,LIME*.1+film(cosI,d),.6);
  vec3 F=F0+(vec3(1.)-F0)*pow(1.-cosI,5.);
  vec3 diffuse=LIME*(.18+.3*max(dot(n,normalize(vec3(-.45,.75,.55))),0.));
  vec3 col=mix(diffuse*2.2,diffuse+studio(r)*F,.7);
  col*=mix(.7,1.,faceMask);
  col=mix(col,INK,edge*.92);
  float band=dot(lp.xy,normalize(vec2(1.,.65)));
  float pos=mix(-1.7,1.7,fract(uAngle/6.2831853));
  col+=mix(vec3(1.),F0*1.4,.6)*exp(-pow((band-pos)/.2,2.))*.42*faceMask;
  return col;
}
vec3 tone(vec3 c){
  float m=max(max(c.r,c.g),c.b);
  float k=m>.8?(.8+.2*(1.-exp(-(m-.8)/.2)))/m:1.;
  vec3 t=c*k;
  t=mix(t,vec3(max(max(t.r,t.g),t.b)),smoothstep(1.6,4.,m)*.35);
  return pow(clamp(t,0.,1.),vec3(1./2.2));
}
vec4 sampleAt(vec2 frag,vec3 ro,vec3 fw,vec3 rt,vec3 up){
  vec2 uv=(frag-.5*uRes)/(.5*uRes.y);
  vec3 rd=normalize(fw+(rt*uv.x+up*uv.y)*(.8/6.));
  float t=march(ro,rd);
  return t>0.?vec4(tone(shade(ro+rd*t,rd)),1.):vec4(0.);
}
void main(){
  gc=cos(uAngle);gs=sin(uAngle);
  vec3 ro=6.*vec3(0.,.1392,.9903);
  vec3 fw=normalize(-ro);
  vec3 rt=normalize(cross(fw,vec3(0.,1.,0.)));
  vec3 up=cross(rt,fw);
  vec2 f=gl_FragCoord.xy;
  gl_FragColor=.25*(sampleAt(f+vec2(.125,.375),ro,fw,rt,up)+sampleAt(f+vec2(-.375,.125),ro,fw,rt,up)
    +sampleAt(f+vec2(.375,-.125),ro,fw,rt,up)+sampleAt(f+vec2(-.125,-.375),ro,fw,rt,up));
}`;

interface Renderer {
  gl: WebGLRenderingContext;
  canvas: HTMLCanvasElement;
  res: WebGLUniformLocation | null;
  angle: WebGLUniformLocation | null;
}

/** `undefined` until first asked for; `null` where WebGL is missing. */
let renderer: Renderer | null | undefined;
const tiles = new Set<HTMLCanvasElement>();
let frame = 0;
let start = 0;

function create(): Renderer | null {
  try {
    const canvas = document.createElement("canvas");
    const gl = canvas.getContext("webgl", { premultipliedAlpha: true });
    if (!gl) return null;
    const shader = (type: number, source: string) => {
      const s = gl.createShader(type)!;
      gl.shaderSource(s, source);
      gl.compileShader(s);
      return s;
    };
    const program = gl.createProgram()!;
    gl.attachShader(program, shader(gl.VERTEX_SHADER, VERT));
    gl.attachShader(program, shader(gl.FRAGMENT_SHADER, FRAG));
    gl.bindAttribLocation(program, 0, "p");
    gl.linkProgram(program);
    if (!gl.getProgramParameter(program, gl.LINK_STATUS)) return null;
    gl.useProgram(program);
    gl.bindBuffer(gl.ARRAY_BUFFER, gl.createBuffer());
    gl.bufferData(
      gl.ARRAY_BUFFER,
      new Float32Array([-1, -1, 3, -1, -1, 3]),
      gl.STATIC_DRAW,
    );
    gl.enableVertexAttribArray(0);
    gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);
    return {
      gl,
      canvas,
      res: gl.getUniformLocation(program, "uRes"),
      angle: gl.getUniformLocation(program, "uAngle"),
    };
  } catch {
    return null;
  }
}

/** One ease-in-out turn, then a hold: the angle at `ms` into the loop. */
function angleAt(ms: number) {
  const k = Math.min(((ms / 1000) % (SPIN + HOLD)) / SPIN, 1);
  const eased = k < 0.5 ? 4 * k * k * k : 1 - (-2 * k + 2) ** 3 / 2;
  return eased * Math.PI * 2;
}

function still() {
  return !!window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
}

function paint(now: number) {
  const r = renderer!;
  const size = Math.max(...[...tiles].map((tile) => tile.width), 1);
  if (r.canvas.width !== size) r.canvas.width = r.canvas.height = size;
  const { gl } = r;
  gl.viewport(0, 0, size, size);
  gl.clearColor(0, 0, 0, 0);
  gl.clear(gl.COLOR_BUFFER_BIT);
  gl.uniform2f(r.res, size, size);
  gl.uniform1f(r.angle, still() ? 0.6 : angleAt(now - start));
  gl.drawArrays(gl.TRIANGLES, 0, 3);
  for (const tile of tiles) {
    const context = tile.getContext("2d");
    if (!context) continue;
    context.clearRect(0, 0, tile.width, tile.height);
    context.drawImage(r.canvas, 0, 0, tile.width, tile.height);
  }
  // reduced motion draws the mark once, at rest
  frame = tiles.size && !still() ? requestAnimationFrame(paint) : 0;
}

/**
 * A spinning mark for a running turn's author line (`.bc-mark-spin`, inside `.bc-mark-tile`), or null where
 * WebGL is missing. `stop` releases it; the loop runs only while some mark spins.
 */
export function spinningMark(): {
  canvas: HTMLCanvasElement;
  stop(): void;
} | null {
  if (renderer === undefined) renderer = create();
  if (!renderer) return null;
  const canvas = document.createElement("canvas");
  canvas.className = "bc-mark-spin";
  canvas.setAttribute("aria-hidden", "true");
  canvas.width = canvas.height = Math.round(
    SIZE * Math.min(window.devicePixelRatio || 1, 2),
  );
  if (!tiles.size) start = performance.now();
  tiles.add(canvas);
  if (!frame) frame = requestAnimationFrame(paint);
  return {
    canvas,
    stop() {
      tiles.delete(canvas);
      if (!tiles.size && frame) {
        cancelAnimationFrame(frame);
        frame = 0;
      }
    },
  };
}
