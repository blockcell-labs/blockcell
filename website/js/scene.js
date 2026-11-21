import * as THREE from 'three';
import { EffectComposer } from '../vendor/jsm/postprocessing/EffectComposer.js';
import { RenderPass } from '../vendor/jsm/postprocessing/RenderPass.js';
import { UnrealBloomPass } from '../vendor/jsm/postprocessing/UnrealBloomPass.js';
import { OutputPass } from '../vendor/jsm/postprocessing/OutputPass.js';

/* ============================================================
   BlockCell — "The Crossing"
   Real-time WebGL hero scene: a Stargate ring with a swirling
   event horizon, block-built Replicator robots gathering to
   cross over, drifting starfield and energy motes.
   ============================================================ */

const canvas = document.getElementById('scene');
const reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;

let renderer, scene, camera, composer, bloomPass;
let gateGroup, innerRing, chevrons = [], portal, portalCore, halo;
let robots = [], motes, starfield, rimLight;
let clock = new THREE.Clock();
let running = true;

const pointer = { x: 0, y: 0, tx: 0, ty: 0 };
const PARAMS = {
  portalTime: { value: 0 },
  portalColorA: { value: new THREE.Color('#1b6cff') },
  portalColorB: { value: new THREE.Color('#66f0ff') },
};

/* ---------- shared geometry / materials ---------- */
const metalDark = new THREE.MeshStandardMaterial({
  color: 0x11151f, metalness: 0.92, roughness: 0.34,
});
const metalMid = new THREE.MeshStandardMaterial({
  color: 0x394457, metalness: 0.95, roughness: 0.28,
});
const chevronOff = new THREE.MeshStandardMaterial({
  color: 0x1a2333, emissive: 0x0a1730, emissiveIntensity: 0.6,
  metalness: 0.8, roughness: 0.4,
});

function init() {
  renderer = new THREE.WebGLRenderer({ canvas, antialias: true, alpha: true, powerPreference: 'high-performance' });
  renderer.setPixelRatio(Math.min(window.devicePixelRatio, 2));
  renderer.setSize(window.innerWidth, window.innerHeight);
  renderer.setClearColor(0x05070e, 0);
  renderer.toneMapping = THREE.ACESFilmicToneMapping;
  renderer.toneMappingExposure = 1.15;
  renderer.outputColorSpace = THREE.SRGBColorSpace;

  scene = new THREE.Scene();
  scene.fog = new THREE.FogExp2(0x05070e, 0.017);

  camera = new THREE.PerspectiveCamera(46, window.innerWidth / window.innerHeight, 0.1, 200);
  camera.position.set(0, 1.6, 17);
  camera.lookAt(0, 1.6, 0);

  /* ---------- lights ---------- */
  scene.add(new THREE.AmbientLight(0x22304a, 1.1));
  const key = new THREE.DirectionalLight(0x8fb6ff, 1.4);
  key.position.set(-6, 9, 8);
  scene.add(key);
  const fill = new THREE.DirectionalLight(0x5a3fff, 0.8);
  fill.position.set(7, 2, -4);
  scene.add(fill);
  rimLight = new THREE.PointLight(0x46d9ff, 3.2, 26, 2);
  rimLight.position.set(0, 1.6, 0.5);
  scene.add(rimLight);

  buildGate();
  buildPortal();
  buildRobots();
  buildStarfield();
  buildMotes();
  setupPost();

  window.addEventListener('resize', onResize);
  window.addEventListener('pointermove', onPointerMove, { passive: true });
  document.addEventListener('visibilitychange', () => { running = !document.hidden; });
  animate();
  document.documentElement.classList.add('scene-ready');
}

/* ---------- STARGATE ---------- */
function buildGate() {
  gateGroup = new THREE.Group();
  gateGroup.position.set(0, 2.0, 0);
  scene.add(gateGroup);

  const R = 4.1;
  // Outer heavy ring
  const outer = new THREE.Mesh(new THREE.TorusGeometry(R, 0.52, 20, 96), metalDark);
  gateGroup.add(outer);
  // Trim rings front & back
  for (const z of [0.34, -0.34]) {
    const trim = new THREE.Mesh(new THREE.TorusGeometry(R, 0.09, 12, 96), metalMid);
    trim.position.z = z;
    gateGroup.add(trim);
  }
  // Inner rotating ring with notches
  innerRing = new THREE.Group();
  const innerTorus = new THREE.Mesh(new THREE.TorusGeometry(R - 0.62, 0.16, 12, 96), metalMid);
  innerRing.add(innerTorus);
  const notchGeo = new THREE.BoxGeometry(0.09, 0.34, 0.28);
  for (let i = 0; i < 44; i++) {
    const a = (i / 44) * Math.PI * 2;
    const n = new THREE.Mesh(notchGeo, i % 4 === 0 ? metalMid : metalDark);
    n.position.set(Math.cos(a) * (R - 0.62), Math.sin(a) * (R - 0.62), 0);
    n.rotation.z = a;
    innerRing.add(n);
  }
  gateGroup.add(innerRing);

  // 9 chevrons — Stargate's signature
  const chevGeo = new THREE.BoxGeometry(0.36, 0.72, 0.34);
  for (let i = 0; i < 9; i++) {
    const a = Math.PI / 2 + (i / 9) * Math.PI * 2;
    const pivot = new THREE.Group();
    pivot.position.set(Math.cos(a) * R, Math.sin(a) * R, 0.42);
    pivot.rotation.z = a - Math.PI / 2;
    const mat = chevronOff.clone();
    const chev = new THREE.Mesh(chevGeo, mat);
    const glow = new THREE.Mesh(
      new THREE.BoxGeometry(0.16, 0.42, 0.1),
      new THREE.MeshBasicMaterial({ color: 0x7fe9ff, transparent: true, opacity: 0 })
    );
    glow.position.z = 0.2;
    pivot.add(chev, glow);
    gateGroup.add(pivot);
    chevrons.push({ mat, glow, index: i });
  }

  // Glowing rim just inside the ring
  halo = new THREE.Mesh(
    new THREE.TorusGeometry(R - 0.86, 0.055, 8, 96),
    new THREE.MeshBasicMaterial({ color: 0x39d4ff, transparent: true, opacity: 0.85 })
  );
  gateGroup.add(halo);
}

/* ---------- EVENT HORIZON (custom shader) ---------- */
function buildPortal() {
  const geo = new THREE.CircleGeometry(3.35, 96);
  const mat = new THREE.ShaderMaterial({
    transparent: true,
    depthWrite: false,
    side: THREE.DoubleSide,
    blending: THREE.AdditiveBlending,
    uniforms: {
      uTime: PARAMS.portalTime,
      uColorA: PARAMS.portalColorA,
      uColorB: PARAMS.portalColorB,
      uReveal: { value: 0 },
    },
    vertexShader: `
      varying vec2 vUv;
      void main() {
        vUv = uv;
        gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0);
      }
    `,
    fragmentShader: `
      uniform float uTime;
      uniform float uReveal;
      uniform vec3 uColorA;
      uniform vec3 uColorB;
      varying vec2 vUv;

      float hash(vec2 p){ return fract(sin(dot(p, vec2(127.1,311.7)))*43758.5453); }
      float noise(vec2 p){
        vec2 i=floor(p), f=fract(p);
        f=f*f*(3.0-2.0*f);
        return mix(mix(hash(i),hash(i+vec2(1,0)),f.x),
                   mix(hash(i+vec2(0,1)),hash(i+vec2(1,1)),f.x),f.y);
      }
      float fbm(vec2 p){
        float v=0.0, a=0.5;
        for(int i=0;i<5;i++){ v+=a*noise(p); p*=2.02; a*=0.5; }
        return v;
      }
      void main(){
        vec2 uv = vUv - 0.5;
        float r = length(uv) * 2.0;
        if(r>1.0) discard;
        float ang = atan(uv.y, uv.x);

        // swirl coordinates
        float swirl = ang + uTime*0.28 + (1.0-r)*2.4;
        vec2 sp = vec2(cos(swirl), sin(swirl)) * (r*3.4);
        float n = fbm(sp + vec2(uTime*0.12, -uTime*0.09));
        float n2 = fbm(sp*2.3 - vec2(uTime*0.2, uTime*0.15));

        // concentric ripples travelling outward
        float ripples = sin(r*26.0 - uTime*3.2 + n*6.0)*0.5+0.5;

        float energy = n*0.65 + n2*0.35;
        energy += ripples*0.22;

        // bright core, darker edge but still luminous
        float core = smoothstep(0.55, 0.0, r);
        float edge = smoothstep(1.0, 0.72, r);
        float brightness = energy * edge * (0.75 + core*0.9);

        vec3 col = mix(uColorA, uColorB, clamp(energy + core*0.5, 0.0, 1.0));
        col += vec3(0.75,0.95,1.0) * pow(core,2.0) * 0.9;

        float alpha = brightness * uReveal;
        gl_FragColor = vec4(col * (0.9 + brightness), alpha);
      }
    `,
  });
  portal = new THREE.Mesh(geo, mat);
  portal.position.z = 0.05;
  gateGroup.add(portal);

  // Solid bright center disc (the "opening")
  portalCore = new THREE.Mesh(
    new THREE.CircleGeometry(0.85, 48),
    new THREE.MeshBasicMaterial({ color: 0xcfefff, transparent: true, opacity: 0.0, blending: THREE.AdditiveBlending, depthWrite: false })
  );
  portalCore.position.z = 0.08;
  gateGroup.add(portalCore);

  // Animate the reveal
  const t0 = performance.now();
  (function reveal() {
    const p = Math.min((performance.now() - t0) / 1800, 1);
    mat.uniforms.uReveal.value = p * p * (3 - 2 * p);
    portalCore.material.opacity = p * 0.55;
    if (p < 1) requestAnimationFrame(reveal);
  })();
}

/* ---------- REPLICATOR ROBOTS ----------
   BlockCell's namesake: simple modular blocks that assemble
   into mechanical life. Each bot is built from small cubes. */
function buildRobots() {
  const configs = [
    // two heroes silhouetted in front of the gate
    { pos: [-2.5, -0.55, 2.0], s: 1.25, hue: 0x7af0ff, phase: 0.0 },
    { pos: [ 2.9, -0.75, 2.6], s: 1.35, hue: 0x9a8cff, phase: 1.4 },
    // supporting replicas flanking and in the foreground
    { pos: [-5.0, -1.9, 4.6], s: 1.0,  hue: 0x4fd8c0, phase: 2.7 },
    { pos: [ 5.4, -2.0, 5.3], s: 1.05, hue: 0xb08dff, phase: 3.9 },
    { pos: [-0.4, -2.9, 7.4], s: 0.85, hue: 0x5fe0ff, phase: 5.1 },
  ];
  configs.forEach((cfg) => robots.push(makeRobot(cfg)));
}

function makeRobot({ pos, s, hue, phase }) {
  const g = new THREE.Group();
  g.position.set(...pos);
  g.scale.setScalar(s);
  scene.add(g);

  const blockMat = new THREE.MeshStandardMaterial({
    color: 0x3b4761, metalness: 0.82, roughness: 0.34,
    emissive: new THREE.Color(hue).multiplyScalar(0.12), emissiveIntensity: 1.0,
  });
  const edgeMat = new THREE.LineBasicMaterial({ color: hue, transparent: true, opacity: 0.85 });
  const glowMat = new THREE.MeshBasicMaterial({ color: hue, transparent: true, opacity: 0.95 });
  const cube = new THREE.BoxGeometry(1, 1, 1);
  const edgeGeo = new THREE.EdgesGeometry(cube);

  const add = (x, y, z, sx, sy, sz, glow = false) => {
    const m = new THREE.Mesh(cube, glow ? glowMat : blockMat);
    m.position.set(x, y, z);
    m.scale.set(sx, sy, sz);
    g.add(m);
    // wireframe edges make the modular blocks legible against the dark scene
    if (!glow) {
      const edges = new THREE.LineSegments(edgeGeo, edgeMat);
      edges.position.copy(m.position);
      edges.scale.copy(m.scale).multiplyScalar(1.01);
      g.add(edges);
    }
    return m;
  };

  // A small quadruped / insectoid Replicator assembled from blocks
  add(0, 0, 0, 1.5, 0.55, 1.0);            // body core
  add(0, 0.35, -0.15, 0.9, 0.45, 0.7);     // upper shell
  add(0, 0.12, 0.62, 0.7, 0.55, 0.5);      // head
  add(-0.22, 0.2, 0.9, 0.16, 0.16, 0.12, true); // eyes
  add(0.22, 0.2, 0.9, 0.16, 0.16, 0.12, true);
  // legs
  const legX = [-0.62, 0.62];
  const legZ = [-0.5, 0.5];
  legX.forEach((lx) => legZ.forEach((lz) => {
    add(lx, -0.45, lz, 0.22, 0.75, 0.22);
    add(lx, -0.85, lz + 0.12, 0.3, 0.16, 0.5);
  }));
  // dorsal blocks (modular cells)
  add(-0.35, 0.62, -0.25, 0.32, 0.28, 0.4);
  add(0.35, 0.62, -0.25, 0.32, 0.28, 0.4);
  const spine = add(0, 0.72, -0.1, 0.18, 0.2, 0.7, true);
  spine.material = glowMat.clone();
  spine.material.opacity = 0.55;

  g.rotation.y = Math.PI + (Math.random() - 0.5) * 0.5; // face the gate (+z toward portal)
  return { group: g, phase, baseY: pos[1], s, glowMat };
}

/* ---------- STARFIELD ---------- */
function buildStarfield() {
  const count = 1400;
  const positions = new Float32Array(count * 3);
  const colors = new Float32Array(count * 3);
  const palette = [new THREE.Color('#bcd6ff'), new THREE.Color('#8fa8ff'), new THREE.Color('#ffffff'), new THREE.Color('#7fe0ff')];
  for (let i = 0; i < count; i++) {
    const r = 24 + Math.random() * 55;
    const th = Math.random() * Math.PI * 2;
    const ph = Math.acos(2 * Math.random() - 1);
    positions[i*3]   = r * Math.sin(ph) * Math.cos(th);
    positions[i*3+1] = r * Math.cos(ph) * 0.6 + 4;
    positions[i*3+2] = r * Math.sin(ph) * Math.sin(th) - 10;
    const c = palette[(Math.random() * palette.length) | 0];
    colors[i*3] = c.r; colors[i*3+1] = c.g; colors[i*3+2] = c.b;
  }
  const geo = new THREE.BufferGeometry();
  geo.setAttribute('position', new THREE.BufferAttribute(positions, 3));
  geo.setAttribute('color', new THREE.BufferAttribute(colors, 3));
  starfield = new THREE.Points(geo, new THREE.PointsMaterial({
    size: 0.16, vertexColors: true, transparent: true, opacity: 0.9,
    sizeAttenuation: true, depthWrite: false,
  }));
  scene.add(starfield);
}

/* ---------- ENERGY MOTES drifting toward the gate ---------- */
function buildMotes() {
  const count = 260;
  const positions = new Float32Array(count * 3);
  const speeds = new Float32Array(count);
  for (let i = 0; i < count; i++) {
    positions[i*3]   = (Math.random() - 0.5) * 30;
    positions[i*3+1] = (Math.random() - 0.5) * 16 + 2;
    positions[i*3+2] = (Math.random() - 0.5) * 22 + 4;
    speeds[i] = 0.3 + Math.random() * 0.9;
  }
  const geo = new THREE.BufferGeometry();
  geo.setAttribute('position', new THREE.BufferAttribute(positions, 3));
  motes = new THREE.Points(geo, new THREE.PointsMaterial({
    color: 0x69e6ff, size: 0.085, transparent: true, opacity: 0.75,
    blending: THREE.AdditiveBlending, depthWrite: false,
  }));
  motes.userData.speeds = speeds;
  scene.add(motes);
}

/* ---------- POST PROCESSING ---------- */
function setupPost() {
  composer = new EffectComposer(renderer);
  composer.addPass(new RenderPass(scene, camera));
  bloomPass = new UnrealBloomPass(
    new THREE.Vector2(window.innerWidth, window.innerHeight),
    0.85,  // strength
    0.55,  // radius
    0.22   // threshold
  );
  composer.addPass(bloomPass);
  composer.addPass(new OutputPass());
}

/* ---------- INTERACTION ---------- */
function onPointerMove(e) {
  pointer.tx = (e.clientX / window.innerWidth) * 2 - 1;
  pointer.ty = (e.clientY / window.innerHeight) * 2 - 1;
}
function onResize() {
  const w = window.innerWidth, h = window.innerHeight;
  camera.aspect = w / h;
  camera.updateProjectionMatrix();
  renderer.setSize(w, h);
  composer.setSize(w, h);
}

/* ---------- LOOP ---------- */
function animate() {
  if (!running) { requestAnimationFrame(animate); return; }
  const t = clock.getElapsedTime();
  const speed = reduceMotion ? 0.25 : 1;

  PARAMS.portalTime.value = t * speed;

  // chevron cascade
  chevrons.forEach((c) => {
    const on = ((t * 1.6 * speed + c.index * 0.7) % 9) < 1.0;
    const target = on ? 1 : 0;
    c.mat.emissive.setHex(on ? 0x2fa8ff : 0x0a1730);
    c.mat.emissiveIntensity += (target * 2.2 + 0.4 - c.mat.emissiveIntensity) * 0.15;
    c.glow.material.opacity += (target * 0.9 - c.glow.material.opacity) * 0.15;
  });

  if (innerRing) innerRing.rotation.z -= 0.03 * speed;
  if (halo) halo.material.opacity = 0.7 + Math.sin(t * 2) * 0.18;
  if (rimLight) rimLight.intensity = 3.0 + Math.sin(t * 1.7) * 0.6;
  if (portalCore) portalCore.scale.setScalar(1 + Math.sin(t * 2.2) * 0.06);

  // gate gentle float
  if (gateGroup) gateGroup.rotation.y = Math.sin(t * 0.25) * 0.06 + pointer.x * 0.12;

  // robots bob and shuffle toward the gate
  robots.forEach((r) => {
    const p = r.phase;
    r.group.position.y = r.baseY + Math.sin(t * 1.3 * speed + p) * 0.12;
    r.group.rotation.y = Math.PI + Math.sin(t * 0.5 + p) * 0.14;
    r.group.rotation.z = Math.sin(t * 0.9 + p) * 0.03;
  });

  // motes flow toward portal center then reset
  const mp = motes.geometry.attributes.position;
  const speeds = motes.userData.speeds;
  for (let i = 0; i < speeds.length; i++) {
    let x = mp.array[i*3], y = mp.array[i*3+1], z = mp.array[i*3+2];
    const d = Math.hypot(x, y - 2.0, z);
    const pull = speeds[i] * 0.02 * speed;
    x -= x * pull; y -= (y - 2.0) * pull; z -= z * pull;
    if (d < 3.4) {
      const ang = Math.random() * Math.PI * 2;
      const rad = 13 + Math.random() * 8;
      x = Math.cos(ang) * rad;
      y = 2 + (Math.random() - 0.5) * 14;
      z = Math.sin(ang) * rad + 3;
    }
    mp.array[i*3] = x; mp.array[i*3+1] = y; mp.array[i*3+2] = z;
  }
  mp.needsUpdate = true;

  if (starfield) starfield.rotation.y += 0.004 * speed;

  // camera parallax
  pointer.x += (pointer.tx - pointer.x) * 0.05;
  pointer.y += (pointer.ty - pointer.y) * 0.05;
  camera.position.x = pointer.x * 1.5;
  camera.position.y = 1.6 - pointer.y * 0.9;
  camera.lookAt(0, 1.7, 0);

  composer.render();
  requestAnimationFrame(animate);
}

// addons are imported via a bootstrap module (main entry) that defines EffectComposer etc.
init();
