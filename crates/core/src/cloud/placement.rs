//! Where a placed scan's points go in the model ([`Placement`]), and the turn that lays one set of points over another best
//! ([`best_turn`], Horn's method with unit quaternions), which `align_cloud` fits a scan's tilt with.

/// A turn in space, as a matrix that takes a point's coordinates to the turned point's.
pub type Turn = [[f64; 3]; 3];

/// Where a placed scan's points go in the model: each turned about the scan's middle by its roll (about its own x), then its
/// pitch (about its own y), then its angle (about the vertical), and moved to `at`, its middle in the model.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub at: [f64; 3],
    turn: Turn,
}

impl Placement {
    /// The placement of a scan whose middle is at `at`, turned `angle` degrees counter-clockwise and tilted by `pitch` and
    /// `roll` degrees.
    pub fn new(at: [f64; 3], angle: f64, pitch: f64, roll: f64) -> Placement {
        let (sa, ca) = angle.to_radians().sin_cos();
        let (sp, cp) = pitch.to_radians().sin_cos();
        let (sr, cr) = roll.to_radians().sin_cos();
        let turn = [
            [ca * cp, ca * sp * sr - sa * cr, ca * sp * cr + sa * sr],
            [sa * cp, sa * sp * sr + ca * cr, sa * sp * cr - ca * sr],
            [-sp, cp * sr, cp * cr],
        ];
        Placement { at, turn }
    }

    /// Where a point of the scan, from its middle, is in the model.
    pub fn place(&self, p: [f64; 3]) -> [f64; 3] {
        std::array::from_fn(|i| self.at[i] + self.turn[i][0] * p[0] + self.turn[i][1] * p[1] + self.turn[i][2] * p[2])
    }

    /// A model point in the scan's own terms: from its middle, turned back.
    pub fn local(&self, p: [f64; 3]) -> [f64; 3] {
        let d: [f64; 3] = std::array::from_fn(|i| p[i] - self.at[i]);
        std::array::from_fn(|k| self.turn[0][k] * d[0] + self.turn[1][k] * d[1] + self.turn[2][k] * d[2])
    }

    /// Its turn.
    pub fn turn(&self) -> Turn {
        self.turn
    }
}

/// `turn` as a scan's angle, pitch and roll, degrees: the vertical turn after the pitch after the roll that make it, the pitch
/// within a quarter turn either way.
pub fn angles(turn: &Turn) -> (f64, f64, f64) {
    let pitch = (-turn[2][0]).clamp(-1.0, 1.0).asin();
    let roll = turn[2][1].atan2(turn[2][2]);
    let angle = turn[1][0].atan2(turn[0][0]);
    (angle.to_degrees(), pitch.to_degrees(), roll.to_degrees())
}

/// `a` then `b`: the turn `b` makes of what `a` turned.
pub fn then(a: &Turn, b: &Turn) -> Turn {
    std::array::from_fn(|i| std::array::from_fn(|k| (0..3).map(|j| b[i][j] * a[j][k]).sum()))
}

/// Point `p` turned by `turn`.
pub fn turned(turn: &Turn, p: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| turn[i][0] * p[0] + turn[i][1] * p[1] + turn[i][2] * p[2])
}

/// The turn that lays the points `from` over the points `to` (each from the middle of its own) best, least squares: Horn's
/// closed form, the unit quaternion that is the eigenvector of the largest eigenvalue of a 4 by 4 matrix of their sums.
pub fn best_turn(from: &[[f64; 3]], to: &[[f64; 3]]) -> Turn {
    let mut s = [[0.0; 3]; 3];
    for (a, b) in from.iter().zip(to) {
        for (i, row) in s.iter_mut().enumerate() {
            for (k, v) in row.iter_mut().enumerate() {
                *v += a[i] * b[k];
            }
        }
    }
    let [[xx, xy, xz], [yx, yy, yz], [zx, zy, zz]] = s;
    let n = [
        [xx + yy + zz, yz - zy, zx - xz, xy - yx],
        [yz - zy, xx - yy - zz, xy + yx, zx + xz],
        [zx - xz, xy + yx, -xx + yy - zz, yz + zy],
        [xy - yx, zx + xz, yz + zy, -xx - yy + zz],
    ];
    let [w, x, y, z] = largest_eigenvector(n);
    [
        [1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y - z * w), 2.0 * (x * z + y * w)],
        [2.0 * (x * y + z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z - x * w)],
        [2.0 * (x * z - y * w), 2.0 * (y * z + x * w), 1.0 - 2.0 * (x * x + y * y)],
    ]
}

/// The eigenvector of symmetric `n`'s largest eigenvalue, of unit length: Jacobi's method, each turn clearing the largest of
/// what lies off the diagonal.
fn largest_eigenvector(mut n: [[f64; 4]; 4]) -> [f64; 4] {
    let mut v: [[f64; 4]; 4] = std::array::from_fn(|i| std::array::from_fn(|k| if i == k { 1.0 } else { 0.0 }));
    let size = n.iter().flatten().fold(0.0, |m: f64, x| m.max(x.abs()));
    for _ in 0..200 {
        let (mut p, mut q, mut big) = (0, 1, 0.0);
        for (i, row) in n.iter().enumerate() {
            for (k, x) in row.iter().enumerate().skip(i + 1) {
                if x.abs() > big {
                    (p, q, big) = (i, k, x.abs());
                }
            }
        }
        if big <= size * 1e-15 || big == 0.0 {
            break;
        }
        let theta = (n[q][q] - n[p][p]) / (2.0 * n[p][q]);
        let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
        let c = 1.0 / (t * t + 1.0).sqrt();
        let s = t * c;
        for row in n.iter_mut().chain(v.iter_mut()) {
            let (a, b) = (row[p], row[q]);
            (row[p], row[q]) = (c * a - s * b, s * a + c * b);
        }
        let (mut rp, mut rq) = (n[p], n[q]);
        for (a, b) in rp.iter_mut().zip(rq.iter_mut()) {
            (*a, *b) = (c * *a - s * *b, s * *a + c * *b);
        }
        (n[p], n[q]) = (rp, rq);
    }
    let best = (0..4).fold(0, |b, i| if n[i][i] > n[b][b] { i } else { b });
    let e: [f64; 4] = std::array::from_fn(|i| v[i][best]);
    let length = e.iter().map(|x| x * x).sum::<f64>().sqrt().max(f64::MIN_POSITIVE);
    e.map(|x| x / length)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f64; 3], b: [f64; 3]) -> bool {
        a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-9)
    }

    #[test]
    fn a_placement_turns_back_what_it_places_and_its_angles_are_what_made_it() {
        let p = Placement::new([100.0, -200.0, 3000.0], 33.0, -12.0, 7.5);
        let q = [1234.0, -567.0, 89.0];
        assert!(close(p.local(p.place(q)), q));
        let (a, pitch, r) = angles(&p.turn());
        assert!((a - 33.0).abs() < 1e-9 && (pitch + 12.0).abs() < 1e-9 && (r - 7.5).abs() < 1e-9, "{a} {pitch} {r}");
    }

    #[test]
    fn the_best_turn_lays_points_over_the_same_points_turned() {
        let turn = Placement::new([0.0; 3], 140.0, 20.0, -35.0).turn();
        let from = [[1000.0, 0.0, 0.0], [0.0, 2000.0, 0.0], [0.0, 0.0, 500.0], [-1000.0, -2000.0, -500.0]];
        let to = from.map(|a| turned(&turn, a));
        let found = best_turn(&from, &to);
        for (a, b) in from.iter().zip(to) {
            assert!(close(turned(&found, *a), b), "{:?} {b:?}", turned(&found, *a));
        }
    }
}
