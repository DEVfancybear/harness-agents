# Memory readout: MEMORILLA in ha

English | [Tiếng Việt](MEMORY_READOUT.vi.md)

**Status:** shipped in 0.8.3; since 0.8.8 it reads words only (embeddings were
removed, see [Memory store §14](MEMORY_STORE.en.md)). This adapts MEMORILLA (Fallahpour, Idehpour,
Kothapalli et al., "Latent semantic memory for LLMs", arXiv 2610.21375,
Stanford) to ha. The code is the authority.

## 1. What the paper does

MEMORILLA gives a frozen LLM access to a whole document collection through a
fixed number of query-dependent "memory tokens":

1. **Encoding, once.** A frozen encoder (Qwen3-Embedding-4B) embeds each
   document, mean-pooled, into a persistent store. Only new or changed
   documents are embedded again.
2. **Query-conditioned retrieval.** For each query, `K` slots (`K = 16`) are
   initialized from learned vectors and conditioned on the query embedding by
   cross-attention. They then attend to every stored embedding through
   `L_ref = 2` cross-attention blocks, Perceiver-style. This is a continuous,
   query-conditioned read of the whole collection, not a discrete top-k
   selection.
3. **Projection.** The refined slots are projected into the frozen LLM's input
   embedding space as `K` soft tokens and placed before the prompt.
4. **Training.** Only the memory module (194 M parameters) is trained, on
   paraphrase reconstruction, then QA and summarization, then the benchmarks.
   For agents (TextWorld), it is trained with GRPO on environment reward,
   reading the agent's past turns as documents.

The paper reports:

- the best average across personalization, long-document and knowledge
  question answering, with 16 tokens in place of up to 50,000;
- recall of personal context that shares no word with the question, for
  example a user's anaemia for a generic question;
- an advantage that grows with the number of entities or facts a question
  combines;
- that retrieval and the latent read answer *different* questions, so their
  union beats either one, which motivates hybrids;
- agents that loop less between explored rooms.

## 2. What cannot carry over

ha drives hosted models through APIs that take text, not input embeddings,
and the machine it runs on need not have a GPU. Soft memory tokens can only be
fed to a local open-weights decoder (the released `memorilla/Memorilla-Qwen3-8B`
with Qwen3-8B behind vLLM's prompt embeddings, about 24 GB of VRAM). The
maintainer chose to adapt the method to ha rather than ship that backend.
Nothing is trained either: the paper reports that an untrained module hurt the
agent, so ha takes the method's structure, not its weights.

## 3. What ha takes

`harness_types::readout` keeps the parts that do not need training or soft
tokens:

| MEMORILLA | ha |
|---|---|
| Frozen encoder, store computed once | TF-IDF vectors of the words, computed per read: weights from the store, no word list. Until 0.8.7 the embeddings the user configured, when there were any. |
| `K` slots, conditioned on the query | Up to `K` slots (1 to 16 for a tool call, 8 by default), each conditioned on the query. |
| Distinct slot identities (learned `Q0`) | Each slot reads what the slots before it left of the query: the query's residual after their reads (Gram-Schmidt). A query about two things puts them in two slots. |
| Cross-attention over every document, `L_ref = 2` | Softmax attention over every vector by cosine similarity (temperature 0.05), two rounds, the query as the residual connection. |
| `K` tokens, whatever the store's size | At most `K` slots, each rendered as the documents carrying 80 % of its attention (at most 3), each document in one slot only. A document is rendered only when its similarity stands one standard deviation above the store's mean: a statistic of the store, so it holds for any encoder's scale. |
| Hybrid with retrieval (future work) | Until 0.8.7 the digest fused a readout by meaning with its ranking by words; since 0.8.8 the digest ranks by words, recency and use. |

## 4. Where it runs

- **The digest every turn carries.** Not any more: until 0.8.7, with
  embeddings configured, the digest fused a readout by meaning with its ranking
  by words and marked such an entry `[recalled by meaning]`. 0.8.8 removed that
  with the embeddings.
- **The `memory` tool, `search` with `mode: readout`.** The same read over the
  entries the filters (`tree`, `meta`, `meta_predicate`, `temporal`, `grep`)
  let through, in `slots` slots (default 8).
  - Each slot lists its entries with their `attention`.
  - The result says `encoder: words`; one that recalls nothing says which
    query it read and to search again with other words.
  - The entries it shows are counted as hits (`memory_usage`).
  - It reads memory only; knowledge is searched with `mode: rank`.
- **`history_search` with `mode: readout`.** The paper's continual setting: the
  conversation's past turns, user and assistant messages and tool results, are
  the documents.
  - The newest 1,500 sources in scope are read, the first 1,500 characters of
    each, with the same window and kind filters.
  - The vectors are TF-IDF vectors of each source's words.
  - Each hit names its `slot` and `attention`, so what was tried, decided or
    seen across a long conversation comes back together, its compacted turns
    included.

## 5. Not taken

- **Soft memory tokens, a trained module, GRPO.** These are out of reach
  through text APIs (see §2).
- **Automatic injection of past turns at every step.** The paper's agent
  memory supplements a full history the model still sees. ha's context already
  carries the history up to compaction. After compaction, the model asks for
  the rest with `history_search` (`mode: readout`), and the compaction summary
  names that tool.

## 6. Evidence

- **Unit tests** cover:
  - the read itself: slots split a two-part query, a fixed budget over 500
    documents, the standout statistic;
  - the tool's readout: a two-part request finds both entries and not an
    unrelated one;
  - the history readout: the sources behind each part of the question come back
    in slots, an unrelated one does not.
- **Live**, `memory-store.sh` section 12 ran on a fake embedding server until
  0.8.7; 0.8.8 keeps its word checks: a readout splits a two-part request.
