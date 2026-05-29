// A selection has a global and local starting/ending point.
// If both points are on the same output, we use entirely monitor-local coordinates
// Otherwise we fall back to floor(topleft) and ceil(bottomright) as in global logical coordinates.
