// Scales scene-linear light by a number of stops. Pixels are premultiplied,
// so scaling color leaves alpha alone.
return vec4<f32>(c.rgb * exp2(exposure), c.a);
