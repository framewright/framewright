---
title: Shader Node Manifest
type: reference
description: Every key a shader plugin's plugin.toml can hold, covering the
  node's identity and shader file, its image inputs, each parameter type with
  its default, units, slider range and limits, and the names ids can't use.
related:
  - framewright/shader-nodes
  - framewright/writing-a-shader-node
---

A shader plugin's `plugin.toml` names the node, points at its shader, and
lists its image inputs and parameters. Framewright reads it when it loads
the plugin and refuses a manifest with a missing key, an unknown key, or a
value of the wrong type, reporting which one.

## The node

The top level of the manifest describes the node:

| Key | Type | Required | Meaning |
|---|---|---|---|
| `id` | String | Yes | The node type's id, which projects save. It must contain a dot, as in `example.tint`. |
| `version` | Whole number | Yes | Starts at 1. |
| `name` | String | Yes | The name in the node library and on the node. |
| `category` | String | Yes | The library group the node appears in, such as `Filter`. |
| `shader` | String | No | The shader file, relative to the plugin folder. The default is `shader.wgsl`. |
| `varies_with_time` | Boolean | No | Whether the output changes over time even with constant parameters. The default is `false`. |
| `inputs` | Array of tables | No | The node's image inputs, written as `[[inputs]]` tables. |
| `params` | Array of tables | No | The node's parameters, written as `[[params]]` tables. |

Every node has one image output.

Keep `version` at 1. Framewright can't open a project saved with an older
version of a plugin node, because a shader plugin has no way to convert the
parameters such a project saved. A project saved with a newer version than
the one installed keeps the node unchanged, and reports it as unavailable
when rendering.

When `varies_with_time` is `true`, Framewright renders the node again at
every frame instead of reusing one result while its inputs and parameters
stay the same. The shader receives no time value, so set the key only when
something the shader reads varies over time in a way Framewright can't see.

## Inputs

Each `[[inputs]]` table adds an image input, in order:

| Key | Type | Meaning |
|---|---|---|
| `id` | String | The name the shader reads the input by, and the name projects save its connection under |
| `name` | String | The label on the node's port |

A node needs at least one input. Without an `[[inputs]]` table, it has one
called `image`, labeled `Image`. The shader reads the first input as `c`,
whatever its `id`, and every other input by its `id`.

## Parameters

Each `[[params]]` table adds a parameter. The inspector shows parameters in
the order the manifest lists them:

| Key | Type | Required | Meaning |
|---|---|---|---|
| `id` | String | Yes | The name the shader reads the value by, and the name projects save it under |
| `name` | String | Yes | The label in the inspector |
| `type` | String | Yes | One of the parameter types in the table of types |
| `default` | Depends on `type` | No | The value of a new node |
| `unit` | String | No | How the value is shown, for `float` and `vec2` |
| `range` | Two numbers | No | The span the slider of a `float` covers |
| `limits` | Two numbers | No | The values Framewright accepts for a `float`, `vec2`, or `color`, checked on each component |
| `section` | String | No | The inspector section the parameter appears in. Without one, it's in the section named after the node. |
| `animatable` | Boolean | No | Whether the value can change over time with keyframes |
| `options` | Array of tables | For `choice` | The choices, each with an `id` and a `name` |

A value outside `range` is still allowed, as long as it is within `limits`.
A `default` outside `limits` makes the manifest invalid. Parameters are
animatable by default, except choices.

### Parameter types

Each type takes its default in its own form:

| Type | `default` | Without a default | In the shader |
|---|---|---|---|
| `float` | A number | `0` | `f32` |
| `int` | A whole number | `0` | `i32` |
| `bool` | `true` or `false` | `false` | `bool` |
| `choice` | The `id` of one option | The first option | `i32`, the position of the option picked, counting from 0 |
| `vec2` | Two numbers, `[x, y]` | `[0, 0]` | `vec2<f32>` |
| `color` | Three or four numbers, `[r, g, b]` or `[r, g, b, a]`, in linear Rec.709 | `[0, 0, 0, 1]` | `vec4<f32>`, unpremultiplied, in the working color space |

A `choice` parameter declares its options like this:

```toml
[[params]]
id = "mode"
name = "Mode"
type = "choice"
default = "screen"
options = [
  { id = "multiply", name = "Multiply" },
  { id = "screen", name = "Screen" },
]
```

The shader reads `mode` as `0` for Multiply and `1` for Screen. Projects
save the option's `id`, so you can reorder or add options without changing
what saved projects mean.

### Units

The `unit` key takes one of these values:

| Value | Shown as |
|---|---|
| `none` | `0.500`, a plain number (the default) |
| `pixels` | `12.0 px` |
| `degrees` | `45.0°` |
| `seconds` | `1.500 s` |
| `decibels` | `-6.0 dB` |
| `stops` | `1.00 EV`, stops of exposure |
| `ratio` | `1.500×` |
| `percent` | `50.0%` |

## Ids

Input and parameter ids become names in the shader, so they follow these
rules:

- They use lowercase letters, digits, and underscores, and start with a
  letter.
- They don't repeat each other: no parameter shares an id with an input or
  with another parameter.
- They avoid the names Framewright declares around the shader: `c`, `pos`,
  `frame`, `base`, `prm`, `params`, `src`, `dst`, and `main`, any name made
  of `e` or `ex` followed by digits, and any name starting with `fw_` or
  `op_`.
- They avoid WGSL keywords and type names, such as `let`, `fn`, `f32`, and
  `vec4`.
