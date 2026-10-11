"""Creates the demo project E:\\ks-demo\\aurora used for README screenshots."""
import math
import os
import shutil
import subprocess
import sys
import zipfile

from PIL import Image

ROOT = sys.argv[1] if len(sys.argv) > 1 else r"E:\ks-demo"
P = os.path.join(ROOT, "aurora")

FILES = {
    "README.md": """# Aurora

A tiny real-time renderer for procedural skies, written in Rust with a TypeScript control panel.

## Features

- **GPU sky model** with physically based scattering
- Hot-reloadable shaders and config
- A web dashboard to tweak parameters live

## Quick start

```sh
cargo run --release -- --scene scenes/polar-night.toml
```

| Scene | Frame time |
|---|---|
| polar-night | 1.8 ms |
| desert-dawn | 2.1 ms |

> Built for the joy of looking up.
""",
    "Cargo.toml": """[package]
name = "aurora"
version = "0.4.2"
edition = "2021"
description = "Real-time procedural skies"
license = "MIT"

[dependencies]
wgpu = "24"
glam = "0.29"
serde = { version = "1", features = ["derive"] }
toml = "0.8"
tracing = "0.1"
""",
    "LICENSE": "MIT License\n\nCopyright (c) 2026 Aurora contributors\n\nPermission is hereby granted, free of charge, to any person obtaining a copy\nof this software and associated documentation files (the \"Software\"), to deal\nin the Software without restriction.\n",
    ".gitignore": "/target\nnode_modules/\n*.log\n",
    "src/main.rs": """use aurora::{config::Config, render::Renderer, sky::SkyModel};
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let config = Config::load("aurora.toml")?;
    let mut renderer = Renderer::new(&config)?;
    let sky = SkyModel::from_scene(&config.scene)?;

    let start = Instant::now();
    loop {
        let t = start.elapsed().as_secs_f32();
        // Animate the sun across the sky and render a frame.
        let sun = sky.sun_direction(t * config.time_scale);
        renderer.render_frame(&sky, sun)?;
        if renderer.should_close() {
            break;
        }
    }
    tracing::info!(frames = renderer.frames(), "bye");
    Ok(())
}
""",
    "src/lib.rs": """//! Aurora: real-time procedural skies.

pub mod config;
pub mod render;
pub mod sky;

/// Crate version, shown in the window title.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
""",
    "src/config.rs": """use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub scene: String,
    pub width: u32,
    pub height: u32,
    #[serde(default = "default_time_scale")]
    pub time_scale: f32,
}

fn default_time_scale() -> f32 {
    1.0
}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&text)?)
    }
}
""",
    "src/sky/mod.rs": """mod scattering;

use glam::Vec3;
pub use scattering::{mie_phase, rayleigh_phase};

/// A sky described by its atmosphere and sun path.
pub struct SkyModel {
    pub turbidity: f32,
    pub ground_albedo: Vec3,
    latitude: f32,
}

impl SkyModel {
    pub fn from_scene(name: &str) -> anyhow::Result<Self> {
        let latitude = match name {
            "polar-night" => 78.2,
            "desert-dawn" => 24.5,
            _ => 45.0,
        };
        Ok(Self { turbidity: 2.5, ground_albedo: Vec3::splat(0.3), latitude })
    }

    /// Direction of the sun at time `t` (hours since midnight).
    pub fn sun_direction(&self, t: f32) -> Vec3 {
        let hour_angle = (t / 24.0) * std::f32::consts::TAU;
        let elevation = (self.latitude.to_radians().cos() * hour_angle.cos()).asin();
        Vec3::new(hour_angle.sin(), elevation.sin(), hour_angle.cos()).normalize()
    }
}
""",
    "src/sky/scattering.rs": """/// Rayleigh phase function for scattering angle cosine `mu`.
pub fn rayleigh_phase(mu: f32) -> f32 {
    3.0 / (16.0 * std::f32::consts::PI) * (1.0 + mu * mu)
}

/// Henyey-Greenstein approximation of Mie scattering.
pub fn mie_phase(mu: f32, g: f32) -> f32 {
    let g2 = g * g;
    (1.0 - g2) / (4.0 * std::f32::consts::PI * (1.0 + g2 - 2.0 * g * mu).powf(1.5))
}
""",
    "src/render/mod.rs": """mod pipeline;

use crate::config::Config;
use crate::sky::SkyModel;
use glam::Vec3;

pub struct Renderer {
    frames: u64,
    pipeline: pipeline::SkyPipeline,
}

impl Renderer {
    pub fn new(config: &Config) -> anyhow::Result<Self> {
        let pipeline = pipeline::SkyPipeline::new(config.width, config.height)?;
        Ok(Self { frames: 0, pipeline })
    }

    pub fn render_frame(&mut self, sky: &SkyModel, sun: Vec3) -> anyhow::Result<()> {
        self.pipeline.upload_uniforms(sky, sun);
        self.pipeline.draw()?;
        self.frames += 1;
        Ok(())
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    pub fn should_close(&self) -> bool {
        self.pipeline.window_closed()
    }
}
""",
    "src/render/pipeline.rs": """use crate::sky::SkyModel;
use glam::Vec3;

/// The full-screen sky pass: one triangle, one fragment shader.
pub struct SkyPipeline {
    width: u32,
    height: u32,
    closed: bool,
}

impl SkyPipeline {
    pub fn new(width: u32, height: u32) -> anyhow::Result<Self> {
        Ok(Self { width, height, closed: false })
    }

    pub fn upload_uniforms(&mut self, sky: &SkyModel, sun: Vec3) {
        // TODO: pack into a uniform buffer once the layout settles.
        let _ = (sky.turbidity, sun);
    }

    pub fn draw(&mut self) -> anyhow::Result<()> {
        // TODO: record the render pass.
        Ok(())
    }

    pub fn window_closed(&self) -> bool {
        self.closed
    }
}
""",
    "shaders/sky.wgsl": """struct Uniforms {
    sun: vec3<f32>,
    turbidity: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

@fragment
fn fs_main(@location(0) dir: vec3<f32>) -> @location(0) vec4<f32> {
    let mu = dot(normalize(dir), u.sun);
    let rayleigh = 0.0596831 * (1.0 + mu * mu);
    let color = vec3<f32>(0.18, 0.42, 0.85) * rayleigh * 12.0;
    return vec4<f32>(color, 1.0);
}
""",
    "scenes/polar-night.toml": "scene = \"polar-night\"\nwidth = 1920\nheight = 1080\ntime_scale = 4.0\n",
    "scenes/desert-dawn.toml": "scene = \"desert-dawn\"\nwidth = 2560\nheight = 1440\n",
    "web/package.json": """{
  "name": "aurora-dashboard",
  "version": "0.4.2",
  "private": true,
  "scripts": { "dev": "vite", "build": "vite build" },
  "dependencies": { "react": "^19.0.0" }
}
""",
    "web/src/App.tsx": """import { useState } from "react";
import { Slider } from "./components/Slider";

export function App() {
  const [turbidity, setTurbidity] = useState(2.5);
  return (
    <main className="dashboard">
      <h1>Aurora</h1>
      <Slider label="Turbidity" min={1} max={10} value={turbidity} onChange={setTurbidity} />
    </main>
  );
}
""",
    "web/src/components/Slider.tsx": """type Props = {
  label: string;
  min: number;
  max: number;
  value: number;
  onChange: (value: number) => void;
};

export function Slider({ label, min, max, value, onChange }: Props) {
  return (
    <label>
      {label}
      <input type="range" min={min} max={max} value={value}
        onChange={(e) => onChange(Number(e.target.value))} />
    </label>
  );
}
""",
    "docs/architecture.md": "# Architecture\n\nThe renderer owns a single full-screen pass. The sky model computes\nthe sun direction and scattering coefficients on the CPU each frame.\n",
    "docs/scenes.md": "# Scenes\n\nScenes live in `scenes/*.toml`. Each sets the latitude, resolution and time scale.\n",
    "scripts/build.ps1": "param([switch]$Release)\n$flags = if ($Release) { '--release' } else { '' }\ncargo build $flags\nnpm --prefix web run build\n",
    "data/benchmarks.csv": "scene,gpu,frame_ms\npolar-night,RTX 4070,1.8\ndesert-dawn,RTX 4070,2.1\npolar-night,RX 7800,1.9\n",
    "notes.txt": "- try Bruneton precomputed tables\n- add moon and stars\n- fix banding at the horizon\n",
}


def make_image(path, w, h):
    img = Image.new("RGB", (w, h))
    px = img.load()
    for y in range(h):
        for x in range(w):
            t = y / h
            band = 0.5 + 0.5 * math.sin(x / w * 6.0 + y / h * 3.0)
            glow = max(0.0, 1.0 - abs(t - 0.35 - 0.12 * math.sin(x / w * 5.0)) * 5.0)
            r = int(10 + 40 * t + 90 * glow * band * 0.6)
            g = int(15 + 30 * t + 230 * glow * band)
            b = int(40 + 90 * (1 - t) + 120 * glow * (1 - band))
            px[x, y] = (min(r, 255), min(g, 255), min(b, 255))
    # ground silhouette
    for x in range(w):
        top = int(h * (0.82 + 0.05 * math.sin(x / w * 9.0) + 0.03 * math.sin(x / w * 23.0)))
        for y in range(top, h):
            px[x, y] = (8, 10, 18)
    img.save(path)


def git(*args):
    subprocess.run(["git", *args], cwd=P, check=True, capture_output=True)


def main():
    if os.path.exists(P):
        shutil.rmtree(P, onerror=lambda f, p, e: (os.chmod(p, 0o700), f(p)))
    for rel, text in FILES.items():
        path = os.path.join(P, *rel.split("/"))
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w", encoding="utf-8", newline="\n") as f:
            f.write(text)
    os.makedirs(os.path.join(P, "assets"), exist_ok=True)
    make_image(os.path.join(P, "assets", "aurora.png"), 640, 360)
    make_image(os.path.join(P, "assets", "thumbnail.png"), 160, 90)
    with zipfile.ZipFile(os.path.join(P, "assets", "fonts.zip"), "w") as z:
        z.writestr("Inter-Regular.ttf", b"\0" * 2048)
        z.writestr("Inter-Bold.ttf", b"\0" * 2048)
        z.writestr("LICENSE.txt", "SIL Open Font License")
    git("init", "-q", "-b", "main")
    git("config", "user.name", "Aurora Dev")
    git("config", "user.email", "dev@example.com")
    git("add", ".")
    git("commit", "-q", "-m", "Initial import")
    # Working-tree changes for the git glyphs.
    with open(os.path.join(P, "src", "render", "pipeline.rs"), "a", encoding="utf-8") as f:
        f.write("\n// Experimental: tone mapping happens in the same pass.\n")
    with open(os.path.join(P, "notes.txt"), "a", encoding="utf-8") as f:
        f.write("- profile the scattering loop\n")
    with open(os.path.join(P, "src", "sky", "stars.rs"), "w", encoding="utf-8") as f:
        f.write("/// A starfield, coming soon.\npub struct Stars;\n")
    os.makedirs(os.path.join(P, "target", "release"), exist_ok=True)
    with open(os.path.join(P, "target", "release", "aurora.exe"), "wb") as f:
        f.write(b"MZ" + b"\0" * 4096)
    with open(os.path.join(P, "build.log"), "w", encoding="utf-8") as f:
        f.write("Compiling aurora v0.4.2\nFinished release in 12.3s\n")
    # A hidden dotfile.
    with open(os.path.join(P, ".env"), "w", encoding="utf-8") as f:
        f.write("AURORA_LOG=info\n")
    subprocess.run(["attrib", "+h", os.path.join(P, ".env")], check=False)
    print("demo ready at", P)


main()
