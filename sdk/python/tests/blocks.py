"""A plugin for the host's tests of the SDK, not for agents: tools whose results are content
blocks. `scripts/guests.sh` builds it and `crates/corduroy-wasm/tests/guest_languages.rs`
loads it.

Built with: corduroy-py build sdk/python/tests/blocks.py --out blocks_py.wasm \
              --capabilities tools
"""

from corduroy_plugin import Image, Plugin, Text, text_of
from wit_world.imports import tools

plugin = Plugin("blocks-py", "0.1.0")


@plugin.tool(description="A caption and an image made of the bytes given.", read_only=True)
def picture(input):
    data = bytes(input.get("bytes", []))
    return [Text(f"a picture of {len(data)} bytes"), Image("image/png", data)]


@plugin.tool(description="One image, on its own.", read_only=True)
def image(_input):
    return Image("image/gif", b"GIF89a")


@plugin.tool(description="A list that is not content blocks, so JSON.", read_only=True)
def numbers(_input):
    return [1, 2, 3]


@plugin.tool(description="Call a tool; return what it said as text, then its blocks as they came.")
def relay(input):
    blocks = tools.call(input["tool"], "{}")
    return [Text(text_of(blocks)), *blocks]
