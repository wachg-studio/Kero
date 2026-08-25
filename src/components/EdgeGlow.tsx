import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { useEffect, useRef, useState } from "react";
import type { EdgeState } from "../types";

type ScreenTranslationItem = {
  text: string;
  x: number;
  y: number;
  width: number;
  height: number;
  fontSize?: number;
};

type ScreenTranslationState = {
  active: boolean;
  loading: boolean;
  items: ScreenTranslationItem[];
};

const vertexSource = `
  attribute vec2 a_position;
  void main() {
    gl_Position = vec4(a_position, 0.0, 1.0);
  }
`;

const fragmentSource = `
  precision highp float;

  uniform vec2 u_resolution;
  uniform float u_time;
  uniform float u_energy;
  uniform float u_opacity;
  uniform vec2 u_pointer;
  uniform float u_pointer_active;
  uniform float u_click;
  uniform vec2 u_mark;
  uniform float u_mark_active;
  uniform float u_blue_mode;

  #define PI 3.14159265359
  #define TAU 6.28318530718

  float roundedBoxSdf(vec2 point, vec2 halfSize, float radius) {
    vec2 inner = abs(point) - halfSize + vec2(radius);
    return min(max(inner.x, inner.y), 0.0) + length(max(inner, 0.0)) - radius;
  }

  float borderProgress(vec2 point, vec2 halfSize) {
    float left = point.x + halfSize.x;
    float right = halfSize.x - point.x;
    float top = point.y + halfSize.y;
    float bottom = halfSize.y - point.y;
    float horizontal = halfSize.x * 2.0;
    float vertical = halfSize.y * 2.0;
    float perimeter = horizontal * 2.0 + vertical * 2.0;

    if (top <= right && top <= bottom && top <= left) return left / perimeter;
    if (right <= bottom && right <= left) return (horizontal + top) / perimeter;
    if (bottom <= left) return (horizontal + vertical + right) / perimeter;
    return (horizontal * 2.0 + vertical + bottom) / perimeter;
  }

  float circularDistance(float a, float b) {
    return abs(fract(a - b + 0.5) - 0.5);
  }

  float fogField(float value, float center, float spread) {
    return exp(-pow(circularDistance(value, center) / spread, 2.0));
  }

  vec3 harmonyMist(float t) {
    // A single continuous, restrained palette avoids hard colour bands.
    vec3 base = vec3(0.57, 0.62, 0.69);
    vec3 amplitude = vec3(0.31, 0.34, 0.33);
    vec3 frequency = vec3(1.00, 1.00, 1.00);
    vec3 phase = vec3(0.03, 0.36, 0.69);
    return base + amplitude * cos(TAU * (frequency * fract(t) + phase));
  }

  vec3 blueMist(float t) {
    float p = fract(t);
    // Keep the blue option as the same mist, not a darker secondary effect.
    vec3 cobalt = vec3(0.04, 0.24, 0.88);
    vec3 royal = vec3(0.05, 0.42, 1.00);
    vec3 azure = vec3(0.06, 0.70, 1.00);
    vec3 ice = vec3(0.66, 0.94, 1.00);
    return mix(mix(cobalt, royal, smoothstep(0.0, 0.42, p)), mix(azure, ice, smoothstep(0.42, 1.0, p)), smoothstep(0.24, 0.76, p));
  }

  vec3 edgeColour(float t) {
    return mix(harmonyMist(t), blueMist(t), u_blue_mode);
  }

  void main() {
    vec2 pixel = gl_FragCoord.xy;
    vec2 centered = pixel - u_resolution * 0.5;
    float smallestSide = min(u_resolution.x, u_resolution.y);
    float inset = clamp(smallestSide * 0.0105, 13.0, 28.0);
    float radius = 0.0;
    vec2 halfSize = u_resolution * 0.5 - vec2(inset);
    float distanceToBorder = abs(roundedBoxSdf(centered, halfSize, radius));
    float leftDistance = centered.x + halfSize.x;
    float rightDistance = halfSize.x - centered.x;
    float topDistance = centered.y + halfSize.y;
    float bottomDistance = halfSize.y - centered.y;
    float colourReach = 132.0;
    float leftWeight = exp(-pow(leftDistance / colourReach, 2.0));
    float rightWeight = exp(-pow(rightDistance / colourReach, 2.0));
    float topWeight = exp(-pow(topDistance / colourReach, 2.0));
    float bottomWeight = exp(-pow(bottomDistance / colourReach, 2.0));
    float totalWeight = max(leftWeight + rightWeight + topWeight + bottomWeight, 0.0001);
    float horizontalPosition = clamp((centered.x + halfSize.x) / (halfSize.x * 2.0), 0.0, 1.0);
    float verticalPosition = clamp((halfSize.y - centered.y) / (halfSize.y * 2.0), 0.0, 1.0);
    vec3 coral = vec3(0.96, 0.27, 0.24);
    vec3 gold = vec3(1.00, 0.72, 0.18);
    vec3 mint = vec3(0.14, 0.86, 0.67);
    vec3 cyan = vec3(0.10, 0.67, 1.00);
    vec3 violet = vec3(0.38, 0.24, 0.92);
    vec3 topColour = mix(coral, gold, smoothstep(0.08, 0.68, horizontalPosition));
    vec3 rightColour = mix(gold, mint, smoothstep(0.05, 0.72, verticalPosition));
    vec3 bottomColour = mix(violet, cyan, smoothstep(0.12, 0.74, horizontalPosition));
    vec3 leftColour = mix(coral, violet, smoothstep(0.16, 0.76, verticalPosition));
    vec3 rainbowColour = (
      leftColour * leftWeight +
      rightColour * rightWeight +
      bottomColour * topWeight +
      topColour * bottomWeight
    ) / totalWeight;
    vec3 blueColour = (
      mix(blueMist(0.72), blueMist(0.18), verticalPosition) * leftWeight +
      mix(blueMist(0.28), blueMist(0.66), verticalPosition) * rightWeight +
      mix(blueMist(0.18), blueMist(0.62), horizontalPosition) * topWeight +
      mix(blueMist(0.62), blueMist(0.28), horizontalPosition) * bottomWeight
    ) / totalWeight;
    // These positions are fixed on screen. At each corner, adjacent colours mix as fog.
    vec3 mistColour = mix(rainbowColour, blueColour, u_blue_mode);
    vec3 pearl = vec3(1.0, 0.987, 0.94);
    // Every mode shares one silhouette. Energy controls presence only, never mist width.
    // Two soft depth fields make the light read as mist entering the screen,
    // rather than as a thin illuminated frame.
    float outerMist = exp(-pow(distanceToBorder / 158.0, 1.72));
    float bodyMist = exp(-pow(distanceToBorder / 58.0, 1.48));
    float nearEdge = exp(-pow(distanceToBorder / 12.0, 1.62));
    // All four sides share this same stable density in every assistant state.
    float hazeTexture = 0.86 + 0.14 * sin(pixel.x * 0.0041 + pixel.y * 0.0022 + distanceToBorder * 0.019) * sin(pixel.x * 0.0017 - pixel.y * 0.0030 - distanceToBorder * 0.011);
    vec3 outerHaze = mix(mistColour, pearl, 0.50);
    vec3 bodyHaze = mix(mistColour, pearl, 0.20);
    vec3 result = outerHaze * outerMist * hazeTexture * 0.50;
    result += bodyHaze * bodyMist * 0.44;
    result += pearl * nearEdge * 0.014;

    // Energy is steady during computer control and makes the same fog silhouette
    // more legible without switching to a separate visual treatment.
    float energyGain = 0.68 + u_energy * 0.52;
    result *= energyGain;
    float alpha = (outerMist * 0.38 * hazeTexture + bodyMist * 0.34 + nearEdge * 0.018) * u_opacity * energyGain;
    vec2 pointerPixel = vec2(u_pointer.x * u_resolution.x, (1.0 - u_pointer.y) * u_resolution.y);
    float pointerDistance = length(pixel - pointerPixel);
    float pointerFog = exp(-pow(pointerDistance / 68.0, 1.42)) * u_pointer_active;
    float pointerCore = exp(-pow(pointerDistance / 20.0, 1.75)) * u_pointer_active;
    float clickProgress = 1.0 - u_click;
    float clickRing = exp(-pow((pointerDistance - (18.0 + clickProgress * 84.0)) / (7.0 + clickProgress * 5.0), 2.0)) * u_click * u_pointer_active;
    float clickFlash = exp(-pow(pointerDistance / (18.0 + clickProgress * 34.0), 1.7)) * pow(u_click, 2.0) * u_pointer_active;
    vec3 pointerColour = edgeColour(fract(u_time * 0.11 + pointerDistance / 260.0));
    result += pointerColour * pointerFog * 0.62;
    result += mix(pointerColour, pearl, 0.52) * pointerCore * 0.82;
    result += pointerColour * clickRing * 1.65;
    result += mix(pointerColour, pearl, 0.48) * clickFlash * 1.7;
    alpha += pointerFog * 0.38 + pointerCore * 0.42 + clickRing * 0.94 + clickFlash * 0.82;

    vec2 markPixel = vec2(u_mark.x * u_resolution.x, (1.0 - u_mark.y) * u_resolution.y);
    vec2 markOffset = pixel - markPixel;
    float markDistance = length(markOffset);
    float markOuter = exp(-pow((markDistance - 24.0) / 2.6, 2.0)) * u_mark_active;
    float markInner = exp(-pow((markDistance - 18.0) / 1.8, 2.0)) * u_mark_active;
    float markCross = max(
      exp(-pow(abs(markOffset.x) / 1.2, 2.0)) * exp(-pow(abs(markOffset.y) / 34.0, 2.0)),
      exp(-pow(abs(markOffset.y) / 1.2, 2.0)) * exp(-pow(abs(markOffset.x) / 34.0, 2.0))
    ) * u_mark_active;
    float markHalo = exp(-pow(markDistance / 66.0, 1.5)) * u_mark_active;
    vec3 markBlue = vec3(0.03, 0.40, 1.00);
    result += markBlue * markHalo * 0.72;
    result += pearl * markOuter * 1.45;
    result += markBlue * (markInner + markCross) * 1.5;
    alpha += markHalo * 0.34 + markOuter * 0.92 + (markInner + markCross) * 0.90;
    gl_FragColor = vec4(result, alpha);
  }
`;

function compileShader(gl: WebGLRenderingContext, type: number, source: string) {
  const shader = gl.createShader(type);
  if (!shader) throw new Error("无法创建着色器");
  gl.shaderSource(shader, source);
  gl.compileShader(shader);
  if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
    throw new Error(gl.getShaderInfoLog(shader) ?? "着色器编译失败");
  }
  return shader;
}

export function EdgeGlow() {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [translation, setTranslation] = useState<ScreenTranslationState>({ active: false, loading: false, items: [] });
  const energyRef = useRef(0);
  const targetOpacityRef = useRef(0);
  const colorModeRef = useRef<"rainbow" | "blue">("rainbow");
  const pointerTargetRef = useRef({ x: 0.5, y: 0.5, active: false, clickedAt: 0 });
  const markTargetRef = useRef({ x: 0.5, y: 0.5, active: false });

  useEffect(() => {
    const hasTauriBridge = "__TAURI_INTERNALS__" in window;
    const noListener = Promise.resolve(() => {});
    const stopListening = hasTauriBridge
      ? listen<EdgeState>("kero:edge-state", ({ payload }) => {
          energyRef.current = Math.max(0, Math.min(1, payload.energy));
          targetOpacityRef.current = payload.active ? 1 : 0;
          colorModeRef.current = payload.colorMode === "blue" ? "blue" : "rainbow";
          if (!payload.active) markTargetRef.current.active = false;
        })
      : noListener;
    const stopPointerListening = hasTauriBridge
      ? listen<{ x: number; y: number; active: boolean; click?: boolean }>("kero:computer-pointer", ({ payload }) => {
          pointerTargetRef.current = {
            x: Math.max(0, Math.min(1, payload.x)),
            y: Math.max(0, Math.min(1, payload.y)),
            active: payload.active,
            clickedAt: payload.click ? performance.now() : pointerTargetRef.current.clickedAt,
          };
        })
      : noListener;
    const stopMarkListening = hasTauriBridge
      ? listen<{ x?: number; y?: number; active: boolean }>("kero:computer-mark", ({ payload }) => {
          markTargetRef.current = {
            x: typeof payload.x === "number" ? Math.max(0, Math.min(1, payload.x)) : markTargetRef.current.x,
            y: typeof payload.y === "number" ? Math.max(0, Math.min(1, payload.y)) : markTargetRef.current.y,
            active: payload.active,
          };
        })
      : noListener;
    const stopTranslationStatus = hasTauriBridge
      ? listen<{ active: boolean; loading: boolean; error?: string | null }>("kero:screen-translation-status", ({ payload }) => {
          setTranslation((current) => ({
            active: payload.active,
            loading: payload.loading,
            items: payload.active ? current.items : [],
          }));
        })
      : noListener;
    const stopTranslationResult = hasTauriBridge
      ? listen<{ active: boolean; items?: ScreenTranslationItem[] }>("kero:screen-translation-result", ({ payload }) => {
          setTranslation({ active: payload.active, loading: false, items: payload.active ? payload.items ?? [] : [] });
        })
      : noListener;
    const previewParams = new URLSearchParams(window.location.search);
    if (import.meta.env.DEV && (previewParams.has("edgePreview") || previewParams.has("pointerPreview"))) {
      targetOpacityRef.current = 1;
      energyRef.current = 0.55;
    }
    if (import.meta.env.DEV && previewParams.has("pointerPreview")) {
      pointerTargetRef.current = { x: 0.5, y: 0.5, active: true, clickedAt: performance.now() };
    }

    const canvas = canvasRef.current;
    if (!canvas) return () => { void stopListening.then((unlisten) => unlisten()); void stopPointerListening.then((unlisten) => unlisten()); void stopMarkListening.then((unlisten) => unlisten()); void stopTranslationStatus.then((unlisten) => unlisten()); void stopTranslationResult.then((unlisten) => unlisten()); };
    const gl = canvas.getContext("webgl", { alpha: true, premultipliedAlpha: false, antialias: false });
    if (!gl) return () => { void stopListening.then((unlisten) => unlisten()); void stopPointerListening.then((unlisten) => unlisten()); void stopMarkListening.then((unlisten) => unlisten()); void stopTranslationStatus.then((unlisten) => unlisten()); void stopTranslationResult.then((unlisten) => unlisten()); };

    let program: WebGLProgram | null = null;
    let animationFrame = 0;
    let opacity = 0;
    let startedAt = performance.now();
    let pointerX = 0.5;
    let pointerY = 0.5;

    try {
      const vertex = compileShader(gl, gl.VERTEX_SHADER, vertexSource);
      const fragment = compileShader(gl, gl.FRAGMENT_SHADER, fragmentSource);
      program = gl.createProgram();
      if (!program) throw new Error("无法创建着色器程序");
      gl.attachShader(program, vertex);
      gl.attachShader(program, fragment);
      gl.linkProgram(program);
      if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
        throw new Error(gl.getProgramInfoLog(program) ?? "着色器链接失败");
      }
      gl.deleteShader(vertex);
      gl.deleteShader(fragment);

      const positionBuffer = gl.createBuffer();
      gl.bindBuffer(gl.ARRAY_BUFFER, positionBuffer);
      gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, -1, 1, 1, -1, 1, 1]), gl.STATIC_DRAW);
      gl.useProgram(program);
      const position = gl.getAttribLocation(program, "a_position");
      gl.enableVertexAttribArray(position);
      gl.vertexAttribPointer(position, 2, gl.FLOAT, false, 0, 0);
    } catch (error) {
      console.error(error);
      return () => { void stopListening.then((unlisten) => unlisten()); void stopPointerListening.then((unlisten) => unlisten()); void stopMarkListening.then((unlisten) => unlisten()); void stopTranslationStatus.then((unlisten) => unlisten()); void stopTranslationResult.then((unlisten) => unlisten()); };
    }

    const resolution = gl.getUniformLocation(program, "u_resolution");
    const time = gl.getUniformLocation(program, "u_time");
    const energy = gl.getUniformLocation(program, "u_energy");
    const opacityUniform = gl.getUniformLocation(program, "u_opacity");
    const pointerUniform = gl.getUniformLocation(program, "u_pointer");
    const pointerActiveUniform = gl.getUniformLocation(program, "u_pointer_active");
    const clickUniform = gl.getUniformLocation(program, "u_click");
    const markUniform = gl.getUniformLocation(program, "u_mark");
    const markActiveUniform = gl.getUniformLocation(program, "u_mark_active");
    const blueModeUniform = gl.getUniformLocation(program, "u_blue_mode");

    // Keep the native edge window hidden until its WebGL pipeline is usable.
    // The MCP bridge waits for this signal instead of exposing WebView startup UI.
    if (hasTauriBridge) void invoke("mark_edge_ready");

    const resize = () => {
      const ratio = Math.min(window.devicePixelRatio || 1, 1.75);
      const width = Math.max(1, Math.floor(window.innerWidth * ratio));
      const height = Math.max(1, Math.floor(window.innerHeight * ratio));
      if (canvas.width !== width || canvas.height !== height) {
        canvas.width = width;
        canvas.height = height;
        gl.viewport(0, 0, width, height);
      }
    };

    const render = (now: number) => {
      resize();
      opacity += (targetOpacityRef.current - opacity) * 0.09;
      pointerX += (pointerTargetRef.current.x - pointerX) * 0.22;
      pointerY += (pointerTargetRef.current.y - pointerY) * 0.22;
      const click = Math.max(0, 1 - (now - pointerTargetRef.current.clickedAt) / 520);
      gl.clearColor(0, 0, 0, 0);
      gl.clear(gl.COLOR_BUFFER_BIT);
      gl.useProgram(program);
      gl.uniform2f(resolution, canvas.width, canvas.height);
      gl.uniform1f(time, (now - startedAt) / 1000);
      gl.uniform1f(energy, energyRef.current);
      gl.uniform1f(opacityUniform, opacity);
      gl.uniform2f(pointerUniform, pointerX, pointerY);
      gl.uniform1f(pointerActiveUniform, pointerTargetRef.current.active ? 1 : 0);
      gl.uniform1f(clickUniform, click);
      gl.uniform2f(markUniform, markTargetRef.current.x, markTargetRef.current.y);
      gl.uniform1f(markActiveUniform, markTargetRef.current.active ? 1 : 0);
      gl.uniform1f(blueModeUniform, colorModeRef.current === "blue" ? 1 : 0);
      gl.drawArrays(gl.TRIANGLES, 0, 6);
      animationFrame = requestAnimationFrame(render);
    };

    const observer = new ResizeObserver(resize);
    observer.observe(document.documentElement);
    animationFrame = requestAnimationFrame(render);

    return () => {
      cancelAnimationFrame(animationFrame);
      observer.disconnect();
      void stopListening.then((unlisten) => unlisten());
      void stopPointerListening.then((unlisten) => unlisten());
      void stopMarkListening.then((unlisten) => unlisten());
      void stopTranslationStatus.then((unlisten) => unlisten());
      void stopTranslationResult.then((unlisten) => unlisten());
      if (program) gl.deleteProgram(program);
    };
  }, []);

  return (
    <main className="edge-layer" aria-hidden="true">
      <canvas ref={canvasRef} className="edge-canvas" />
      {translation.active && (
        <section className="screen-translation-layer">
          {translation.items.map((item, index) => (
            <span
              className="screen-translation-item"
              key={`${index}-${item.text}`}
              style={{
                left: `${item.x * 100}%`,
                top: `${item.y * 100}%`,
                width: `${item.width * 100}%`,
                minHeight: `${item.height * 100}%`,
                fontSize: `${item.fontSize ?? 14}px`,
              }}
            >
              {item.text}
            </span>
          ))}
          {translation.loading && <div className="screen-translation-loading"><i /><span>翻译中</span></div>}
        </section>
      )}
    </main>
  );
}
