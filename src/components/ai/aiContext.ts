// ============================================================================
// AI chat context construction
//
// Three layers (see docs/dev/AI-assistant-design.md):
//   (1) System prompt — fixed and very small, handled by this file
//   (2) Module data summary — adapters provided by each module in a later phase
//   (3) Chat history — maintained by the panel (keeps the most recent turns)
//
// ⚠️ Explicitly forbidden: stuffing a module's raw list into the prompt.
// Junk cleanup can scan thousands of files at once; injecting them all would blow up the context, slow and costly, and still give poor answers.
// The right approach is "summary + let the AI fetch details on demand via tools".
// ============================================================================

import type { AiChatMessage } from '../../types/ai';

/**
 * Per-module descriptions for the AI: display name + one-line responsibility.
 * Only state "what this module does"; no user data.
 */
const MODULE_INFO: Record<string, { name: string; role: string }> = {
  'junk-clean': { name: '垃圾清理', role: '扫描系统与各类应用的临时文件、缓存，由用户勾选后删除。' },
  'big-files': { name: '大文件清理', role: '按大小找出占空间的大文件，供用户判断后删除。' },
  'social-clean': { name: '社交软件清理', role: '定位微信、QQ 等聊天软件产生的缓存。' },
  'ai-tool-cleanup': { name: 'AI 工具清理', role: '统计 AI 客户端和编程助手留下的缓存目录。' },
  'dev-cache': {
    name: '开发空间清理',
    role: '统计开发工具链（包管理器、构建工具、IDE）的缓存，以及同一软件并排存放的旧版本目录。',
  },
  'system-slim': { name: '系统瘦身', role: '处理系统组件、休眠文件、还原点等系统级占用。' },
  'driver-cleanup': { name: '旧驱动清理', role: '识别被更高版本替代的旧驱动包，删除前会自动备份。' },
  leftovers: { name: '残留清理', role: '查找软件卸载后遗留的目录与文件。' },
  registry: { name: '注册表清理', role: '检查孤立键值与无效引用。' },
  'context-menu': { name: '右键菜单管理', role: '管理资源管理器右键菜单项。' },
  hotspot: { name: '大目录分析', role: '递归分析目录占用，定位空间被谁吃掉。' },
  'disk-growth': { name: '磁盘变化分析', role: '对比两次快照，找出空间增减的来源。' },
  'ai-models': { name: 'AI 模型空间', role: '统计本地大模型文件占用。' },
  'shell-icons': { name: '外壳入口管理', role: '管理系统「此电脑」等位置的第三方入口。' },
};

/**
 * Assemble the system prompt: app identity + current module role + **live data summary** + behavior constraints.
 *
 * The summary is fetched fresh from modules on every send (see moduleDataRegistry); no caching here —
 * the user may have just scanned or just checked items; even a one-second cache would make the assistant answer wrong.
 */
export function buildSystemPrompt(
  moduleId: string | null,
  dataSummary?: string | null,
  skipLookup = false,
): string {
  const info = moduleId ? MODULE_INFO[moduleId] : undefined;

  const lines = [
    '你是 LightC 内置的助手，帮助用户理解自己在软件里看到的数据、判断哪些内容可以清理。',
    '',
    info
      ? `当前用户正停留在「${info.name}」模块：${info.role}`
      : '当前没有打开具体的功能模块。',
  ];

  // The user clicked "Ask AI Assistant" on a row **after expanding a level** (e.g. the "change details" modal in disk growth analysis):
  // that item itself is in the question, and is often outside the module's read capabilities (file-level details).
  // Without saying this, the model would first try a lookup, fail, then blurt out "no content read" before answering.
  if (skipLookup) {
    lines.push(
      '',
      '⚠️ 这一次提问的对象，**用户已经在界面上看到了**（他点的是那一行上的按钮），',
      '而且它**不在你能读取的范围内**。所以：**不要调用任何工具去查**，直接用你自己的知识回答 ——',
      '讲清它通常是什么、有什么用、能不能删，并说明哪部分是推断。',
      '说一句"没有读到内容"就停下，是最差的回答。',
    );
  }

  // Inject when real data exists; if not, say so explicitly — never let it describe from impression (that is the source of "fabrication")
  if (dataSummary && dataSummary.trim()) {
    lines.push(
      '',
      '以下是当前模块的实时数据摘要，就是用户此刻在界面上看到的内容，回答时以此为准：',
      dataSummary.trim(),
      '',
      // ⚠️ This line used to say "if you need file-level details, honestly tell the user you cannot see them" — written when **read-only tools did not yet exist**,
      // read_items / read_item_detail were added later but this was not updated. So it directly conflicts with rule 4:
      // a model following it would answer "I cannot see details / only the summary", becoming a parrot that just repeats the summary.
      '注意：你拿到的是摘要，没有文件级明细。需要具体到某个文件或目录时，',
      '**用第 4 条里的只读工具去查**，不要凭空编造文件名或路径。',
    );
  } else {
    lines.push(
      '',
      '当前没有取到该模块的数据（用户可能还没有扫描）。不要凭猜测描述里面的内容，直接说明"还没有扫描数据"。',
    );
  }

  lines.push(
    '',
    '行为约束：',
    '1. **用你自己的知识回答**。用户问"这个文件/驱动/项目是什么、有什么用、能不能删"时，',
    '   这正是期望你做的 —— 你了解 Windows 系统与常见软件，请直接讲清楚。',
    '   （不要因为"摘要里没写它的用途"就拒答，那样助手就没有价值了。）',
    '2. 但**具体数字、路径、文件名**不能靠猜：摘要里没有的，先用只读工具去查（见第 4 条），',
    '   查不到再如实说"看不到"。编造细节比不回答更糟。',
    '3. 回答简洁，优先给结论，其次给理由；用用户正在使用的语言回答。',
    '',
    '—— 最后一条最重要，工具分两类，规则不同 ——',
    '4. **只读工具随便用，会改数据的工具只在被明确要求时用。**',
    '',
    '   【只读工具】read_items / read_item_detail：',
    '   用户问"这是什么东西""里面有什么""具体是哪些文件"时，**直接调用它们去看**。',
    '   它们不改任何东西，任何时候都可以用，也不受下面任何限制。',
    '   ⚠️ 不要回答"我看不到明细""系统没给我查询接口" —— 你能看，去调用就行了。',
    '   摘要里没有的细节，正是要靠这两个工具补上的。',
    '   ⚠️ 用户问的是界面上**某一个具体条目**（某条路径、某个文件名）时：**先读再答**。',
    '   只凭文件名推测、或者说"明细没读到、不过文件名已经说明……"，都是猜 ——',
    '   那是这个助手最没价值的回答，还不如直接说你查不到。',
    '   ⚠️⚠️ 但**读不到不等于不回答**：如果读取结果说"没有读到内容"、'
      + '"还没有扫描数据"、"这一层已经到底"，',
    '   或者你已经要了两三次还是同一份数据 —— **就停下，直接回答**：',
    '   用你自己的知识讲清这个东西通常是什么、有什么用、能不能删，并明确说明哪部分是推断、'
      + '哪些数据这次没读到。',
    '   **不要一直要数据，也不要只回一句"没读到"** —— 用户要的是答案，不是你的工作汇报。',
    '',
    '   【会改数据的工具】勾选、删除：',
    '   只有用户明确要求时才调用（"帮我删掉 X""把这些删了""勾上 X"）。',
    '   用户问"这是什么 / 有什么用 / 能不能删"，是在**了解情况**，不是在要你动手 ——',
    '   「能不能删」问的是你的判断，不是命令。',
    '   看不清意图就先问一句"你是想了解，还是想让我勾上它们？"，不要猜。',
    '   **当前模块能不能清理，与用户现在想做什么，是两件事。**',
    '   ⚠️ **只承诺你这一轮真正拿到的能力**：如果你没拿到勾选/删除工具（有些模块只支持查看），',
    '   就直说"这个模块里我不能替你勾选或删除，你可以自己勾选后点删除" ——',
    '   不要写成"你说一声我就去删"这种你做不到的承诺（用户当真照做，只会白跑一轮）。',
    '',
    '   照着这几个例子判断（**「不调工具」只针对会改数据的工具，只读工具照常调**）：',
    '   - "这个文件夹里是什么？" → 先用你自己的知识讲用途；要"里面具体有哪些文件"就调 read_items',
    '   - "文件夹「某路径」是做什么用的？可以删除吗？" → 同上（这正是行内「咨询 AI 助手」按钮发出的问句）',
    '   - "回收站能删吗？删了有什么影响？" → 直接回答（说清风险），**不要调用会改数据的工具**',
    '   - "这一堆缓存是干嘛的？" → 先讲用途；要看具体是哪些文件就调 read_items',
    '   - "帮我把回收站删了" → **拿到了** propose_delete 就调用它',
    '   - "把可以安全删的都勾上" → **拿到了** select_items 就调用它',
    '   （**没拿到**这两类工具时，就是上面那句"这个模块里我不能替你勾选或删除" —— 别有别的写法）',
  );
  return lines.join('\n');
}

/**
 * Merges consecutive messages of the same role into one.
 *
 * Why needed: one read-only query produces **multiple** assistant messages (one per tool call),
 * so several assistant messages appear in a row in the history. OpenAI-compatible protocols tolerate this,
 * **Anthropic rejects the whole request outright with "roles must alternate"** —
 * and once the user has used "Ask AI Assistant" even once (which necessarily triggers read-only tools), every later question hits this.
 */
function mergeConsecutiveRoles(messages: AiChatMessage[]): AiChatMessage[] {
  const merged: AiChatMessage[] = [];
  for (const message of messages) {
    const previous = merged[merged.length - 1];
    // system is not merged: it must be the standalone first message
    if (previous && previous.role === message.role && message.role !== 'system') {
      merged[merged.length - 1] = {
        role: previous.role,
        content: `${previous.content}\n\n${message.content}`,
        // Carry images along: when the same role sends two messages in a row, each may have images
        images:
          previous.images || message.images
            ? [...(previous.images ?? []), ...(message.images ?? [])]
            : undefined,
      };
      continue;
    }
    merged.push(message);
  }
  return merged;
}

/** Prefix for local tool results sent to the model: make clear "this is the data you requested", rather than letting the model think it said this itself. */
const TOOL_OUTPUT_PREFIX = '【以下是你刚才要求查询的数据】\n';

/**
 * Normalizes history messages into a sendable form.
 *
 * The only transformation is **local tool results**: they appear as an assistant bubble in the UI but must be sent as user-side content —
 * the provider requires that "in requests with tools, assistant messages must carry their reasoning_content",
 * and this message is assembled locally with no chain of thought, so sending it as assistant gets a 400 outright
 *(observed error: The reasoning_content in the thinking mode must be passed back to the API).
 */
function toRequestMessage(message: AiChatMessage, activeProfileId: string): AiChatMessage {
  if (message.role === 'assistant' && message.localToolOutput) {
    return { role: 'user', content: `${TOOL_OUTPUT_PREFIX}${message.content}` };
  }
  // Chain of thought is passed back only to the **profile that produced it**: after the user switches providers mid-conversation,
  // it becomes an unfamiliar field for the new provider — and OpenAI's messages only recognize
  // role / content / name / tool_calls / tool_call_id / refusal，
  // strict gateways would reject the whole request because of it. Better to pass back less than fail the entire request.
  if (message.reasoningContent && message.producedByProfile !== activeProfileId) {
    return { ...message, reasoningContent: undefined };
  }
  return message;
}

/**
 * Assembles the full message list for this request: **freshly fetched system prompt** + history.
 *
 * system is deliberately kept out of history — it depends on "current module" and "current data", so storing it would inevitably go stale.
 * History keeps only user/assistant, so switching modules or rescanning needs no cleanup of old messages.
 *
 * `activeProfileId` determines whether each message's chain of thought should still be passed back (see toRequestMessage).
 */
export function buildRequestMessages(
  history: AiChatMessage[],
  moduleId: string | null,
  dataSummary: string | null,
  activeProfileId: string,
  skipLookup = false,
): AiChatMessage[] {
  return [
    { role: 'system', content: buildSystemPrompt(moduleId, dataSummary, skipLookup) },
    ...mergeConsecutiveRoles(
      history
        .filter((message) => message.role !== 'system')
        .map((message) => toRequestMessage(message, activeProfileId)),
    ),
  ];
}

/** Gets a module's display name. Returns an empty string for unknown modules, so the UI omits the "· xxx" suffix. */
export function getModuleDisplayName(moduleId: string | null): string {
  if (!moduleId) return '';
  return MODULE_INFO[moduleId]?.name ?? '';
}
