"""Edit this mapping to call your Python scientific implementation."""
from decimal import Decimal
from model import double, transform_csv
from protocol import MAX_FILE_BYTES, ModelError, publish, staged_dataset


def evaluate(request, workspace, mode):
    if mode == "json":
        if request["input_files"] or request["inputs"].keys() != {"value"} or not isinstance(request["inputs"]["value"], Decimal):
            raise ModelError("One finite numeric value is required.")
        return {"doubled": double(request["inputs"]["value"])}, {}
    count, content = transform_csv(staged_dataset(request, workspace))
    publish(workspace / "processed.csv", content, MAX_FILE_BYTES)
    return {"sample_count": count}, {"processed_dataset": {
        "path": "processed.csv", "media_type": "text/csv", "scientific_metadata": {}}}
