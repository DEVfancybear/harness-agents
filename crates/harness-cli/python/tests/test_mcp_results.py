"""Both generic connections and Python integrations retain MCP content."""
import sys
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from rlm import mcp, mcp_base


class Block(SimpleNamespace):
    def model_dump(self, **kwargs):
        return vars(self)


class McpResultTests(unittest.TestCase):
    def test_mixed_content_retains_text_and_images(self):
        blocks = [Block(type="text", text="screenshot description"),
                  Block(type="image", data="fixture", mimeType="image/png")]
        result = SimpleNamespace(content=blocks, is_error=False, structured_content=None)
        for parser in (mcp._parse_result, mcp_base._parse_result):
            with self.subTest(parser=parser.__module__):
                self.assertEqual(parser(result), [vars(b) for b in blocks])

    def test_text_errors_and_empty_structured_results_keep_their_contract(self):
        result = SimpleNamespace(content=[Block(type="text", text="first"), Block(type="text", text="second")],
                                 is_error=False, structured_content=None)
        for parser in (mcp._parse_result, mcp_base._parse_result):
            with self.subTest(parser=parser.__module__):
                self.assertEqual(parser(result), "first\nsecond")
                result.structured_content = {}
                self.assertEqual(parser(result), {})
                result.is_error = True
                with self.assertRaisesRegex(mcp_base.McpToolError, "first"):
                    parser(result)
                result.is_error = False
                result.structured_content = None


class McpSearchTests(unittest.IsolatedAsyncioTestCase):
    async def test_configured_connections_are_searched_without_hiding_failed_connections(self):
        inventory = [{"connectionId": "local", "status": "configured"},
                     {"connectionId": "offline", "status": "not_connected"}]
        async def dispatch(operation):
            return await operation()
        with patch.object(mcp, "list_connections", AsyncMock(return_value=inventory)), \
             patch.object(mcp, "_dispatch", dispatch), \
             patch.object(mcp._registry, "search", AsyncMock(return_value=[{"connectionId": "local", "name": "echo"}])) as search:
            result = await mcp.search_tools("echo")
        self.assertEqual(result["searched"], ["local"])
        self.assertEqual(result["tools"][0]["name"], "echo")
        search.assert_awaited_once_with("local", "echo", 20)


if __name__ == "__main__":
    unittest.main()
