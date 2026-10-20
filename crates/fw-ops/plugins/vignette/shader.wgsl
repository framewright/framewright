// Darkens towards the edges. Distance is measured from `centre` (0 to 1
// across the image) and is 100% at the corners of a centred frame.
let extent = frame.zw - frame.xy;
let aspect = vec2<f32>(extent.x / max(extent.y, 1e-6), 1.0);
let along = (pos - frame.xy) / extent;
let d = length((along - centre) * aspect) / length(0.5 * aspect);
let outer = size / 100.0;
let inner = outer * (1.0 - softness / 100.0);
let falloff = smoothstep(inner, max(outer, inner + 1e-4), d);
return vec4<f32>(c.rgb * (1.0 - amount / 100.0 * falloff), c.a);
