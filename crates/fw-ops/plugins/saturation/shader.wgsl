// Moves colour towards or away from its own luminance, in the working space
// (ACEScg, whose luminance weights these are).
let luma = dot(c.rgb, vec3<f32>(0.2722287, 0.6740818, 0.0536895));
return vec4<f32>(mix(vec3<f32>(luma), c.rgb, saturation / 100.0), c.a);
