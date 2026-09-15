"""SmolVLM-500M-Instruct als Triton-Python-Backend (Demo).

Ein Aufruf = ein Bild + eine Frage -> eine kurze Antwort. Die Generierung ist
ein zusammenhaengender GPU-Block ohne Unterbrechungspunkt: genau das Verhalten,
das neben einem getakteten Detektor Frische kostet.

Eingaben:  IMAGE  UINT8 [H, W, 3] (RGB)
           PROMPT BYTES [1]
           MAX_TOKENS INT32 [1] (optional, Vorgabe 24)
Ausgabe:   TEXT   BYTES [1]

Gewichte: /weights (HuggingFaceTB/SmolVLM-500M-Instruct, Apache-2.0).
"""

import json

import numpy as np
import torch
import triton_python_backend_utils as pb_utils
from PIL import Image
from transformers import AutoModelForImageTextToText, AutoProcessor

WEIGHTS = "/weights"
DEFAULT_MAX_TOKENS = 24


class TritonPythonModel:
    def initialize(self, args):
        self.device = "cuda"
        # Das cuDNN im vLLM-Image findet fuer die Patch-Faltung des Bildencoders
        # auf der RTX 3070 keine Engine ("GET was unable to find an engine").
        # Ohne cuDNN rechnet PyTorch diese eine Faltung selbst; der Rest des
        # Modells ist Attention und Matrixmultiplikation und davon unberuehrt.
        torch.backends.cudnn.enabled = False
        # Ein Bild, keine Kachelung: sonst waechst die Zahl der Bildtoken mit der
        # Aufloesung, und die Dauer eines Aufrufs haengt am Eingabeformat.
        self.processor = AutoProcessor.from_pretrained(WEIGHTS)
        try:
            self.processor.image_processor.do_image_splitting = False
            self.processor.image_processor.size = {"longest_edge": 512}
        except AttributeError:
            pass
        self.model = AutoModelForImageTextToText.from_pretrained(
            WEIGHTS, torch_dtype=torch.bfloat16
        ).to(self.device)
        self.model.eval()
        # Aufwaermen, damit der erste echte Aufruf nicht die Kernel-Kompilierung misst.
        self._generate(np.zeros((512, 512, 3), dtype=np.uint8), "Describe the image.", 4)

    def _generate(self, image, prompt, max_tokens):
        messages = [
            {
                "role": "user",
                "content": [{"type": "image"}, {"type": "text", "text": prompt}],
            }
        ]
        text = self.processor.apply_chat_template(messages, add_generation_prompt=True)
        inputs = self.processor(
            text=text, images=[Image.fromarray(image, "RGB")], return_tensors="pt"
        ).to(self.device)
        with torch.inference_mode():
            out = self.model.generate(
                **inputs, max_new_tokens=int(max_tokens), do_sample=False
            )
        new_tokens = out[0, inputs["input_ids"].shape[1]:]
        torch.cuda.synchronize()
        return self.processor.decode(new_tokens, skip_special_tokens=True).strip()

    def execute(self, requests):
        responses = []
        for request in requests:
            try:
                image = pb_utils.get_input_tensor_by_name(request, "IMAGE").as_numpy()
                prompt_raw = pb_utils.get_input_tensor_by_name(request, "PROMPT").as_numpy()
                prompt = prompt_raw.reshape(-1)[0]
                if isinstance(prompt, bytes):
                    prompt = prompt.decode("utf-8")
                max_tokens = DEFAULT_MAX_TOKENS
                mt = pb_utils.get_input_tensor_by_name(request, "MAX_TOKENS")
                if mt is not None:
                    max_tokens = int(mt.as_numpy().reshape(-1)[0])
                if image.ndim != 3 or image.shape[2] != 3:
                    raise ValueError(f"IMAGE must be [H, W, 3], got {list(image.shape)}")
                answer = self._generate(image.astype(np.uint8), prompt, max_tokens)
                out = pb_utils.Tensor("TEXT", np.array([answer.encode("utf-8")], dtype=object))
                responses.append(pb_utils.InferenceResponse(output_tensors=[out]))
            except Exception as error:  # noqa: BLE001 - jede Anfrage bekommt eine Antwort
                responses.append(
                    pb_utils.InferenceResponse(error=pb_utils.TritonError(str(error)))
                )
        return responses

    def finalize(self):
        self.model = None
