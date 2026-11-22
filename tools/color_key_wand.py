"""Color "magic wand" for the passthrough color key.

Learns which colors belong to an object (e.g. skin) from labeled screenshots and writes the RGBA
color table (LUT) used by the headset. Screenshots are in the same color space as the LUT input
(checked on Quest 3: a threshold in the LUT shows up at the same value in screenshots).
"""
import numpy as np
from PIL import Image, ImageDraw, ImageFilter

RES = 32


def load(path):
    return np.asarray(Image.open(path).convert('RGB'))


def polygon_mask(shape, polygon, erode=0):
    img = Image.new('L', (shape[1], shape[0]), 0)
    ImageDraw.Draw(img).polygon([tuple(p) for p in polygon], fill=255)
    for _ in range(erode):
        img = img.filter(ImageFilter.MinFilter(3))
    return np.asarray(img) > 0


def smooth3(h, sigma):
    r = int(np.ceil(3 * sigma))
    k = np.exp(-0.5 * (np.arange(-r, r + 1) / sigma) ** 2)
    k /= k.sum()
    for axis in range(3):
        h = np.apply_along_axis(lambda v: np.convolve(np.pad(v, r, mode='edge'), k, 'valid'), axis, h)
    return h


def histogram(pixels, sigma):
    """Normalized color density, indexed [b][g][r] like the LUT"""
    idx = np.rint(pixels.astype(float) * (RES - 1) / 255).astype(int)
    h = np.zeros((RES, RES, RES))
    np.add.at(h, (idx[:, 2], idx[:, 1], idx[:, 0]), 1)
    h /= max(len(pixels), 1)
    return smooth3(h, sigma)


def build_alpha(foreground, background, sigma=0.8, prior=0.3, softness=1.0):
    """Probability that a color is foreground (0..1), per LUT texel"""
    f = histogram(foreground, sigma) * prior
    b = histogram(background, sigma) * (1 - prior)
    # Unseen colors are background
    ratio = np.log(f + 1e-9) - np.log(b + 1e-7)
    return 1 / (1 + np.exp(-np.clip(ratio / softness, -30, 30)))


def to_lut_bytes(alpha):
    v = np.rint(np.arange(RES) * 255 / (RES - 1)).astype(np.uint8)
    lut = np.zeros((RES, RES, RES, 4), np.uint8)
    lut[..., 0] = v[None, None, :]
    lut[..., 1] = v[None, :, None]
    lut[..., 2] = v[:, None, None]
    lut[..., 3] = np.rint(alpha * 255).astype(np.uint8)
    return lut.tobytes()


def apply_alpha(image, alpha):
    """Trilinear lookup, like the compositor"""
    p = image.reshape(-1, 3).astype(np.float32) * (RES - 1) / 255
    i0 = np.minimum(p.astype(int), RES - 2)
    t = p - i0
    out = np.zeros(len(p), np.float32)
    for db in (0, 1):
        for dg in (0, 1):
            for dr in (0, 1):
                w = (np.abs(1 - dr - t[:, 0]) * np.abs(1 - dg - t[:, 1]) * np.abs(1 - db - t[:, 2]))
                out += w * alpha[i0[:, 2] + db, i0[:, 1] + dg, i0[:, 0] + dr]
    return out.reshape(image.shape[:2])
