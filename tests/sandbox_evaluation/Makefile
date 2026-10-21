ROOT_DIR:=$(shell git rev-parse --show-toplevel)

.PHONY: gather_bash_calls test

gather_bash_calls:
	python3 export_copilot_bash.py \
        --workspace $(ROOT_DIR) \
        --output data/copilot-bash-commands-$(shell date +%s).json

%.report.json: %.json
	cargo run --quiet --example bash_policy_replay -- \
          $< \
          --rules $(ROOT_DIR)/.ylva/profiles/other/config.toml \
          > $@

test:
	python3 -B -m unittest discover -s . -p 'test_*.py' -q && \
	cargo test --example bash_policy_replay
