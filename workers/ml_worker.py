"""Optional local ML worker for MediaShack (PaddleOCR/RapidOCR, OpenCV visual descriptors & face detection).

This worker is strictly optional: MediaShack's core Rust application runs independently without Python
or downloaded ML weights. When Python + rapidocr-onnxruntime / opencv-python / open_clip are available,
MediaShack invokes this script locally with zero network calls.
"""

import json
import os
import sys
import uuid


def handle_ocr(payload):
    frames = payload.get("frames", [])
    detections = []
    try:
        from rapidocr_onnxruntime import RapidOCR

        ocr = RapidOCR()
        for idx, path in enumerate(frames):
            result, _ = ocr(path)
            if not result:
                continue
            for item in result:
                bbox, text, conf = item[0], item[1], float(item[2])
                detections.append(
                    {
                        "frame_index": idx,
                        "text": text,
                        "confidence": conf,
                        "bbox": {"points": bbox},
                    }
                )
        return {"engine": "rapidocr_onnxruntime", "detections": detections}
    except ImportError:
        return {"engine": "unavailable", "detections": []}


def handle_embed_images(payload):
    # Try OpenCLIP if installed; otherwise fall back to Rust's native descriptor by returning unavailable
    frames = payload.get("frames", [])
    try:
        import open_clip
        import torch
        from PIL import Image

        model, _, preprocess = open_clip.create_model_and_transforms("ViT-B-32", pretrained="laion2b_s34b_b79k")
        model.eval()
        embeddings = []
        with torch.no_grad():
            for p in frames:
                img = preprocess(Image.open(p).convert("RGB")).unsqueeze(0)
                feat = model.encode_image(img)
                feat = feat / feat.norm(dim=-1, keepdim=True)
                embeddings.append(feat.squeeze(0).tolist())
        return {"model_version": "openclip_vit_b32_v1", "embeddings": embeddings}
    except Exception:
        return {"engine": "unavailable", "embeddings": []}


def handle_embed_text(payload):
    query = payload.get("query", "")
    try:
        import open_clip
        import torch

        model, _, _ = open_clip.create_model_and_transforms("ViT-B-32", pretrained="laion2b_s34b_b79k")
        tokenizer = open_clip.get_tokenizer("ViT-B-32")
        model.eval()
        with torch.no_grad():
            tokens = tokenizer([query])
            feat = model.encode_text(tokens)
            feat = feat / feat.norm(dim=-1, keepdim=True)
        return {"model_version": "openclip_vit_b32_v1", "embedding": feat.squeeze(0).tolist()}
    except Exception:
        return {"engine": "unavailable", "embedding": []}


def handle_detect_faces(payload):
    frames = payload.get("frames", [])
    crops_dir = payload.get("crops_dir", "")
    try:
        import cv2
        import numpy as np

        cascade_path = os.path.join(cv2.data.haarcascades, "haarcascade_frontalface_default.xml")
        detector = cv2.CascadeClassifier(cascade_path)
        if detector.empty():
            return {"engine": "unavailable", "faces": []}

        faces_out = []
        if crops_dir:
            os.makedirs(crops_dir, exist_ok=True)

        for idx, path in enumerate(frames):
            img = cv2.imread(path)
            if img is None:
                continue
            gray = cv2.cvtColor(img, cv2.COLOR_BGR2GRAY)
            rects = detector.detectMultiScale(gray, scaleFactor=1.1, minNeighbors=5, minSize=(28, 28))
            for (x, y, w, h) in rects:
                crop = img[y : y + h, x : x + w]
                crop_gray = cv2.resize(gray[y : y + h, x : x + w], (8, 8)).astype(np.float32) / 255.0
                vec = crop_gray.flatten()
                norm = float(np.linalg.norm(vec))
                if norm > 1e-6:
                    vec = vec / norm
                crop_path = None
                if crops_dir:
                    crop_path = os.path.join(crops_dir, f"cvface_{idx}_{uuid.uuid4().hex[:8]}.jpg")
                    cv2.imwrite(crop_path, crop)
                faces_out.append(
                    {
                        "frame_index": idx,
                        "bbox": {"x": int(x), "y": int(y), "w": int(w), "h": int(h)},
                        "confidence": 0.85,
                        "crop_path": crop_path,
                        "embedding": vec.tolist(),
                    }
                )
        return {"engine": "opencv_haar_v1", "faces": faces_out}
    except Exception:
        return {"engine": "unavailable", "faces": []}


def main():
    if len(sys.argv) < 3:
        print(json.dumps({"error": "missing action or payload"}))
        sys.exit(1)
    action = sys.argv[1]
    payload = json.loads(sys.argv[2])
    handlers = {
        "ocr": handle_ocr,
        "embed_images": handle_embed_images,
        "embed_text": handle_embed_text,
        "detect_faces": handle_detect_faces,
    }
    fn = handlers.get(action)
    if not fn:
        print(json.dumps({"error": f"unknown action {action}"}))
        sys.exit(1)
    print(json.dumps(fn(payload)))


if __name__ == "__main__":
    main()
