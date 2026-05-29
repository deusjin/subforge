#!/usr/bin/env python3
"""
SUBFORGE transcription + segmentation sidecar.

Pipeline:
1. faster-whisper: word-level timestamps
2. SaT (Segment Any Text, EMNLP 2024): semantic segmentation with length constraints
3. Optional: BERT-based punctuation restoration (felflare/bert-restore-punctuation)
4. Map segments back to word timestamps

Output: JSON with `words` and `segments` arrays.

Args (JSON via stdin):
{
  "audio": "path/to/audio.wav",
  "model": "small.en" | "/path/to/local/model",
  "language": "en" | null,
  "segmenter": "sat" | "rule",
  "max_chars": 84,
  "target_chars": 42,
  "restore_punctuation": false
}
"""
import json
import sys
import os
import warnings

# Suppress noisy library output
os.environ["TRANSFORMERS_VERBOSITY"] = "error"
os.environ["TOKENIZERS_PARALLELISM"] = "false"
warnings.filterwarnings("ignore")
import logging
logging.getLogger("transformers").setLevel(logging.ERROR)
logging.getLogger("wtpsplit").setLevel(logging.ERROR)


def transcribe_words(audio_path: str, model: str, language: str | None, device: str = "auto", compute_type: str = "auto"):
    """Run faster-whisper, return list of {word, start, end}."""
    from faster_whisper import WhisperModel
    # device='auto' lets ctranslate2 pick CUDA if available, else CPU
    wm = WhisperModel(model, device=device, compute_type=compute_type)
    segments, _ = wm.transcribe(audio_path, word_timestamps=True, language=language)
    words = []
    for seg in segments:
        if seg.words:
            for w in seg.words:
                words.append({
                    "word": w.word,
                    "start": w.start,
                    "end": w.end,
                })
    return words


def detect_language(words: list[dict]) -> str:
    """Heuristic language detection from words for SaT."""
    if not words:
        return "en"
    sample = "".join(w["word"] for w in words[:100])
    cjk = sum(1 for ch in sample if '\u4e00' <= ch <= '\u9fff')
    kana = sum(1 for ch in sample if '\u3040' <= ch <= '\u30ff')
    hangul = sum(1 for ch in sample if '\uac00' <= ch <= '\ud7af')
    total = len(sample)
    if total == 0:
        return "en"
    if cjk / total > 0.2:
        return "zh"
    if kana / total > 0.2 or (cjk > 0 and kana > 0):
        return "ja"
    if hangul / total > 0.2:
        return "ko"
    return "en"


def restore_punctuation(text: str) -> str:
    """Optional BERT-based punctuation restoration for English ASR output."""
    try:
        from transformers import pipeline
    except ImportError:
        return text
    try:
        pipe = pipeline(
            "token-classification",
            model="felflare/bert-restore-punctuation",
            aggregation_strategy="first",
        )
        # Process in chunks to avoid sequence length limits
        chunks = [text[i:i + 1500] for i in range(0, len(text), 1500)]
        result = []
        for chunk in chunks:
            tokens = pipe(chunk)
            rebuilt = ""
            cursor = 0
            for tok in tokens:
                rebuilt += chunk[cursor:tok["end"]]
                label = tok["entity_group"]
                if label.endswith("PERIOD"):
                    rebuilt += "."
                elif label.endswith("COMMA"):
                    rebuilt += ","
                elif label.endswith("QUESTION"):
                    rebuilt += "?"
                cursor = tok["end"]
            rebuilt += chunk[cursor:]
            result.append(rebuilt)
        return "".join(result)
    except Exception as e:
        print(f"punctuation restoration failed: {e}", file=sys.stderr)
        return text


def segment_with_sat(words: list[dict], lang: str, max_chars: int, target_chars: int):
    """Run SaT segmentation, map segments back to word timestamps."""
    from wtpsplit import SaT

    # Prefer TED LoRA for English (specifically tuned for ASR-style transcribed speech)
    # ted2020-corrupted = trained on TED talks with corrupted punctuation, ideal for ASR output
    # See Frohmann et al., EMNLP 2024, Section 5
    sat = None
    if lang == "en":
        try:
            sat = SaT("sat-3l", style_or_domain="ted2020-corrupted", language="en")
        except Exception as e:
            print(f"TED LoRA unavailable ({e}), using sat-3l-sm", file=sys.stderr)
    if sat is None:
        sat = SaT("sat-3l-sm")

    # Concatenate words preserving original spacing (whisper words already include leading space)
    full_text = "".join(w["word"] for w in words)

    # Length-constrained segmentation with lognormal prior (right-skewed: prefer longer segments)
    # This is more tolerant of natural speech rhythm than Gaussian
    raw_segments = sat.split(
        full_text,
        max_length=max_chars,
        prior_type="lognormal",
        prior_kwargs={
            "target_length": target_chars,
            "spread": max(20, target_chars // 2),
            "lang_code": lang,
        },
    )

    # Map each segment back to word range using character offsets
    segments = []
    cursor = 0
    word_idx = 0
    word_char_starts = []
    pos = 0
    for w in words:
        word_char_starts.append(pos)
        pos += len(w["word"])

    for seg_text in raw_segments:
        if not seg_text.strip():
            cursor += len(seg_text)
            continue
        seg_start_char = cursor
        seg_end_char = cursor + len(seg_text)

        first_word = word_idx
        while first_word < len(words) and word_char_starts[first_word] < seg_start_char:
            first_word += 1
        if first_word > 0 and (first_word >= len(words) or word_char_starts[first_word] > seg_start_char):
            if word_char_starts[first_word - 1] >= seg_start_char - 2:
                first_word = max(first_word - 1, word_idx)

        last_word = first_word
        while last_word + 1 < len(words) and word_char_starts[last_word + 1] < seg_end_char:
            last_word += 1

        if first_word >= len(words):
            cursor += len(seg_text)
            continue

        segments.append({
            "text": seg_text.strip(),
            "start": words[first_word]["start"],
            "end": words[last_word]["end"],
            "word_start": first_word,
            "word_end": last_word,
        })
        word_idx = last_word + 1
        cursor += len(seg_text)

    # Post-process: fix segments ending on incomplete markers
    segments = fix_bad_boundaries(segments, max_chars)
    return segments


# Words that a sentence/clause should NOT end on (incomplete thought)
# Source: linguistic intuition + subtitle industry guidelines (BBC, Netflix)
BAD_ENDING_WORDS = {
    # Articles
    "the", "a", "an",
    # Pronouns/determiners (when they're heads of noun phrases)
    "this", "that", "these", "those", "my", "your", "his", "her", "its", "our", "their",
    # Prepositions
    "of", "in", "on", "at", "by", "for", "with", "to", "from", "as", "into", "onto", "about",
    # Conjunctions
    "and", "or", "but", "so", "yet", "nor", "if", "while", "when", "where", "because", "since",
    # Auxiliaries
    "is", "are", "was", "were", "be", "been", "being", "have", "has", "had",
    "do", "does", "did", "will", "would", "shall", "should", "may", "might", "must", "can", "could",
}


def fix_bad_boundaries(segments: list[dict], max_chars: int) -> list[dict]:
    """Merge adjacent segments when the first ends on a bad word.
    
    Subtitle convention: a cue should end on a complete unit. Ending mid-clause
    on words like 'the', 'and', 'our' looks unnatural.
    """
    if len(segments) < 2:
        return segments

    result = []
    i = 0
    while i < len(segments):
        cur = dict(segments[i])  # copy
        # Try to merge while current ends on bad word AND merged length <= max_chars * 1.5
        while i + 1 < len(segments):
            cur_text = cur["text"].strip()
            last_token = cur_text.rstrip(",.;:!?\"'").split()[-1].lower() if cur_text else ""
            if last_token not in BAD_ENDING_WORDS:
                break
            next_seg = segments[i + 1]
            merged_len = len(cur["text"]) + 1 + len(next_seg["text"])
            # Allow up to 1.5x max for the sake of complete clauses
            if merged_len > max_chars * 1.5:
                break
            cur["text"] = cur["text"].rstrip() + " " + next_seg["text"].lstrip()
            cur["end"] = next_seg["end"]
            cur["word_end"] = next_seg.get("word_end", cur.get("word_end"))
            i += 1
        result.append(cur)
        i += 1
    return result


def segment_with_rule(words: list[dict], max_words: int = 12):
    """Fallback: rule-based segmentation by punctuation + max words."""
    segments = []
    current = []

    for w in words:
        current.append(w)
        trimmed = w["word"].strip()
        sentence_end = trimmed.endswith(".") or trimmed.endswith("!") or trimmed.endswith("?")
        at_max = len(current) >= max_words

        if sentence_end or at_max:
            text = "".join(x["word"] for x in current).strip()
            if text:
                segments.append({
                    "text": text,
                    "start": current[0]["start"],
                    "end": current[-1]["end"],
                })
            current = []

    if current:
        text = "".join(x["word"] for x in current).strip()
        if text:
            segments.append({
                "text": text,
                "start": current[0]["start"],
                "end": current[-1]["end"],
            })
    return segments


def main():
    args = json.loads(sys.stdin.read())

    audio = args["audio"]
    model = args["model"]
    language = args.get("language")  # None = auto-detect
    segmenter = args.get("segmenter", "sat")
    max_chars = args.get("max_chars", 84)
    target_chars = args.get("target_chars", 42)
    do_restore_punct = args.get("restore_punctuation", False)
    device = args.get("device", "auto")
    device_display = args.get("device_display", device)
    compute_type = args.get("compute_type", "auto")

    # 1. Transcribe with word timestamps
    print(f"transcribing (device={device_display})...", file=sys.stderr)
    words = transcribe_words(audio, model, language, device=device, compute_type=compute_type)
    if not words:
        print(json.dumps({"words": [], "segments": []}))
        return

    # 2. Optional punctuation restoration (English-only currently)
    detected_lang = detect_language(words)
    if do_restore_punct and detected_lang == "en":
        print("restoring punctuation...", file=sys.stderr)
        full_text = "".join(w["word"] for w in words)
        restored = restore_punctuation(full_text)
        # Re-attach restored punctuation to words by character alignment
        # Simple heuristic: reapply to last word in each sentence
        # For now, just replace the full text (segmentation will use it)
        if restored and len(restored) >= len(full_text) - 10:
            # Only use restored text if reasonable
            words = realign_words_with_text(words, restored)

    # 3. Segment
    print(f"segmenting with {segmenter}...", file=sys.stderr)
    if segmenter == "sat":
        try:
            segments = segment_with_sat(words, detected_lang, max_chars, target_chars)
        except Exception as e:
            print(f"SaT failed ({e}), falling back to rule-based", file=sys.stderr)
            segments = segment_with_rule(words)
    else:
        segments = segment_with_rule(words)

    # 4. Output
    print(json.dumps({
        "words": words,
        "segments": segments,
        "language": detected_lang,
    }))


def realign_words_with_text(words: list[dict], new_text: str) -> list[dict]:
    """Re-attach punctuation from `new_text` (a re-punctuated version of the joined words)
    back to the word list, preserving each word's original timestamp.

    Approach:
    - Tokenize new_text by whitespace into "tokens".
    - Walk both word list and tokens in parallel, matching by alpha prefix.
    - Replace word.text with the matched token (which may now have punctuation).
    - On mismatch (re-punctuator hallucinated/dropped a word), fall back to original.
    """
    if not words or not new_text:
        return words

    new_tokens = new_text.split()
    if not new_tokens:
        return words

    def alpha_lower(s: str) -> str:
        return "".join(ch for ch in s.lower() if ch.isalpha())

    result = []
    j = 0  # cursor in new_tokens
    for w in words:
        original_text = w["word"]
        original_alpha = alpha_lower(original_text)

        # Skip non-alpha words (spaces, pure punctuation) — keep as-is
        if not original_alpha:
            result.append(dict(w))
            continue

        # Try to match within a small window in new_tokens
        matched = None
        for k in range(j, min(j + 3, len(new_tokens))):
            if alpha_lower(new_tokens[k]) == original_alpha:
                matched = k
                break

        if matched is not None:
            # Use the new token (with restored punctuation), but preserve leading space
            leading = " " if original_text.startswith(" ") else ""
            new_word = dict(w)
            new_word["word"] = leading + new_tokens[matched]
            result.append(new_word)
            j = matched + 1
        else:
            # No match — keep original word, advance j conservatively
            result.append(dict(w))

    return result


if __name__ == "__main__":
    main()
