// One minus each channel, in linear light. On premultiplied pixels that's
// alpha minus the channel.
return vec4<f32>(c.a - c.rgb, c.a);
