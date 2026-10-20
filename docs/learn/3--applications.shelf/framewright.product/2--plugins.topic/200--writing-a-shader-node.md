---
title: Writing a Shader Node
type: how-to
description: Write a Tint node as a shader plugin, from an empty folder to a
  node in the node library, then change its shader while Framewright runs
  and read the error Framewright reports for a broken one.
related:
  - framewright/shader-nodes
---

This page builds a Tint node as a shader plugin: a node that multiplies the
image by a color, with a control for how strongly. When you finish, Tint is
in Framewright's node library, and editing a text file changes what it does.
[Shader Nodes](~framewright/shader-nodes) explains what a shader node can
do and what its function receives.

## Before you start

You need:

- Framewright, with a graph or a clip to try the node on
- A text editor

Knowing some WGSL helps, but this page doesn't depend on it.

## Create the plugin

1. Create a folder for the plugin:

   ```sh
   mkdir -p ~/.local/share/framewright/plugins/tint
   ```

2. In that folder, create `plugin.toml` with this content:

   ```toml
   id = "example.tint"
   version = 1
   name = "Tint"
   category = "Filter"

   [[params]]
   id = "tint"
   name = "Tint"
   type = "color"
   default = [1.0, 0.6, 0.3]

   [[params]]
   id = "amount"
   name = "Amount"
   type = "float"
   unit = "percent"
   default = 50
   range = [0, 100]
   limits = [0, 100]
   ```

   The manifest names the node, puts it in the library's **Filter** group,
   and gives it two parameters. `range` sets what the slider covers, and
   `limits` sets the values Framewright accepts. Every node type needs its
   own `id`, so start yours with a prefix of your own and a dot; the
   built-in nodes use `fw.`.

3. In the same folder, create `shader.wgsl` with this content:

   ```wgsl
   // Multiply the image by the tint color, then blend by the amount.
   let tinted = c.rgb * tint.rgb;
   return vec4<f32>(mix(c.rgb, tinted, amount / 100.0), c.a);
   ```

   `c` is the pixel from the node's input. Each parameter is available by
   its `id`: `tint` is a `vec4<f32>` and `amount` is an `f32`.

If Framewright is running, it loads the plugin within about a second of the
save. Otherwise it loads the plugin when it starts.

## Add the node

1. Open the graph you want to tint on the **Compose** page. For a clip on
   the timeline, select the clip and press <kbd>Enter</kbd>.
2. Move the pointer over an empty part of the node editor and press
   <kbd>Tab</kbd>.
3. Enter `tint` and press <kbd>Enter</kbd>. Tint appears where the
   library opened.
4. Drag from the output port of the node that feeds **Output** to the input
   port of **Tint**.
5. Drag from the output port of **Tint** to the input port of **Output**.
   The connection replaces the one that was there.

## Check the result

The viewer shows the image with a warm tint. Select **Tint** in the node
editor: the inspector shows an orange swatch for **Tint** and a slider for
**Amount** at `50.0%`. Drag **Amount** to `100.0%` and the image takes on
the full tint.

## Change the shader

Framewright reads a plugin again whenever one of its files changes, so you
can work on the shader with the viewer open:

1. In `shader.wgsl`, change `tint.rgb` to `tint.bgr`.
2. Save the file.

Within about a second, the viewer shows the image with a cool tint instead,
because the shader swaps the tint's red and blue.

## Read an error

When the shader has an error, Framewright still loads the node and reports
where the error is. To see one, change the second line to multiply by
`tint` rather than `tint.rgb`:

```wgsl
let tinted = c.rgb * tint;
```

When you save, the bar at the bottom of the window shows the error, and the
viewer shows it over the image:

```text
/home/<user-name>/.local/share/framewright/plugins/tint: its shader doesn't work: shader.wgsl:2:14: Operation Multiply can't work with vec3<f32> and vec4<f32>
```

Here *user-name* is your user name. `shader.wgsl:2:14` is the line and
column where the error starts. `c.rgb` has three components and `tint` has
four, so WGSL can't multiply them. Change `tint` back to `tint.rgb` and
save: the error clears and the tint returns.
