---
title: Shader Nodes
type: concept
description: How a shader node turns a manifest and a WGSL function body
  into a node, what the function receives for each pixel, where Framewright
  finds plugins, how it reloads them, and how it reports their errors.
related:
  - framewright/writing-a-shader-node
---

A shader node is a node you write yourself as a short GPU program, without
rebuilding Framewright. It lives in a folder that holds two files: a
manifest that names the node and lists its inputs and parameters, and a
function body in the WebGPU Shading Language (WGSL) that works out one pixel.
Framewright's own Exposure, Saturation, Invert, Vignette, and Mix nodes are
shader nodes, built the same way.

If you have written a DCTL for DaVinci Resolve or a BlinkScript kernel for
Nuke, a shader node will be familiar: a small function, run for every pixel
on the GPU, whose parameters appear as controls.

## How a shader node runs

Framewright runs the function once for every pixel of every frame. The
function sees one pixel of each input, all at the same position, and returns
the pixel the node outputs. It can't read neighboring pixels, so a shader
node can change color and combine images but can't blur or distort them.

Framewright joins neighboring per-pixel steps into one GPU pass. A chain of
shader nodes, together with the color conversions around it, reads and
writes each pixel once, so a shader node costs about as much as the
arithmetic it does.

The manifest and the shader's text are part of the key of every cached frame
the node contributes to. Editing either file changes those keys, so
Framewright never shows a frame made with an older version of the shader.

## What the function sees

The shader file holds the inside of a function. Before its first line,
Framewright declares these names:

| Name | WGSL type | Holds |
|---|---|---|
| `c` | `vec4<f32>` | The pixel from the first input |
| Each other input's id | `vec4<f32>` | The pixel from that input |
| Each parameter's id | Set by the parameter's type | The parameter's value at this frame |
| `pos` | `vec2<f32>` | The pixel's center, in canvas units |
| `frame` | `vec4<f32>` | The image's edges: left, top, right, bottom |

The function returns a `vec4<f32>`: the pixel the node outputs. To get a
position that runs from 0 to 1 across the image, divide `pos - frame.xy` by
`frame.zw - frame.xy`, as the built-in Vignette does.

Each parameter type arrives in the shader as a WGSL type:

| Type in the manifest | WGSL type | Value |
|---|---|---|
| `float` | `f32` | The number |
| `int` | `i32` | The whole number |
| `bool` | `bool` | `true` or `false` |
| `choice` | `i32` | The position of the option picked, counting from 0 |
| `vec2` | `vec2<f32>` | The two numbers |
| `colour` | `vec4<f32>` | Red, green, blue, and alpha, in the working color space |

## Color and alpha

Every input pixel arrives in Framewright's working color space, ACEScg, from
the Academy Color Encoding System (ACES). ACEScg holds scene-linear light:
doubling a value doubles the light, and values above 1.0 are ordinary
highlights rather than errors. Framewright converts each input to ACEScg
before the node, and converts the node's output on to wherever it goes next.

Pixels are premultiplied: their red, green, and blue are already multiplied
by their alpha. Scaling red, green, and blue together works directly on a
premultiplied pixel and leaves its edges clean. A curve, such as a gamma,
needs the unpremultiplied color, so divide by alpha first and multiply by
it again after.

A color parameter is the exception. It arrives unpremultiplied, converted to
the working color space, with its alpha in the fourth component.

## Where Framewright finds plugins

A plugin folder is any folder that holds a `plugin.toml`. Framewright reads
plugins in this order:

1. Its built-in shader nodes.
2. Each plugin folder in `~/.local/share/framewright/plugins`. If the
   `XDG_DATA_HOME` environment variable is set, Framewright uses
   `$XDG_DATA_HOME/framewright/plugins` instead.
3. Each plugin folder in each folder that `FW_PLUGIN_PATH` lists, separated
   by colons. A folder in the list can also be a plugin folder itself.

Every node type needs its own id. If a plugin uses an id that Framewright
has already loaded, Framewright skips the plugin and reports it, so a plugin
can't replace a built-in node.

## Changes while Framewright runs

Framewright watches its plugin folders. When you save a file in one, it
reads every plugin again within about a second and renders the viewer
again. Frames that don't involve a changed plugin stay in the cache.

## Errors

Framewright reports plugin problems in the bar at the bottom of the window,
and on the standard error stream when it was started from a terminal:

- **A manifest that can't be read.** Framewright skips the plugin and
  reports what is wrong with the manifest.
- **A shader with an error.** Framewright still loads the node, so projects
  that use it keep it. The report gives the line and column in the shader
  file, and rendering the node shows the same error in the viewer. Saving a
  fixed shader clears the report.
- **A plugin that isn't installed.** A project that uses its node keeps the
  node and its settings, saves them unchanged, and reports the node as
  unavailable when rendering. After you install the plugin, open the project
  again to use the node.

## What a shader node can't do

A shader node:

- Reads only the pixel at its own position in each input
- Has one image output, and needs at least one image input
- Holds its whole program in the function body, without functions of its
  own
