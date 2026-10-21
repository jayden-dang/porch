# 11. Human voice keeps the engine wire

Operators read `rules` and `reader` on every human surface, while `review.engine` stays `quality`, `agent`, `generic`, or `ocr` because 0.2.3 already wrote that field. A new `review.assurance` value (`rules-only` or `rules+reader`) wins when both keys exist. Setup writes both. Renaming the wire would reject every installed config. Dropping the new key would leave `config.yaml` saying `engine: quality` after the screen stopped using that word.
