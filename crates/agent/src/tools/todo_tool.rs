use std::sync::Arc;

use acp_thread::{AcpThread, PlanEntry};
use agent_client_protocol as acp;
use anyhow::Result;
use chrono::Utc;
use gpui::{App, AppContext, SharedString, Task, WeakEntity};
use language_model::LanguageModelToolResultContent;
use log;
use markdown::Markdown;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{AgentTool, Thread, Todo, TodoPriority, TodoStatus, ToolCallEventStream};

/// Use this tool to create and manage a structured task list for your current coding session.
/// This helps you track progress, organize complex tasks, and demonstrate thoroughness to the user.
///
/// ## When to Use This Tool
/// Use this tool proactively in these scenarios:
/// 1. Complex multistep tasks - When a task requires 3 or more distinct steps or actions
/// 2. Non-trivial and complex tasks - Tasks that require careful planning or multiple operations
/// 3. User explicitly requests todo list - When the user directly asks you to use the todo list
/// 4. User provides multiple tasks - When users provide a list of things to be done
/// 5. After receiving new instructions - Immediately capture user requirements as todos
/// 6. After completing a task - Mark it complete and add any new follow-up tasks
/// 7. When you start working on a task, mark the todo as in_progress. Only have one todo as in_progress at a time.
///
/// ## When NOT to Use This Tool
/// Skip using this tool when:
/// 1. There is only a single, straightforward task
/// 2. The task is trivial and tracking it provides no organizational benefit
/// 3. The task can be completed in less than 3 trivial steps
///
/// ## Task States
/// - pending: Task not yet started
/// - in_progress: Currently working on (limit to ONE task at a time)
/// - completed: Task finished successfully
///
/// ## Important Usage Notes
/// - ALWAYS read the todo list at the beginning of conversations to see what's pending
/// - ALWAYS use this tool when the user asks about previous tasks or plans
/// - ALWAYS update status to "in_progress" when you START working on a task
/// - ALWAYS update status to "completed" IMMEDIATELY after finishing a task
/// - Only have ONE task in_progress at any time - complete existing tasks before starting new ones
///
/// ## Examples:
/// - Read all todos: {"command": "read_todos"}
/// - Add a todo: {"command": "add_todo", "text": "Fix the login bug", "priority": "high"}
/// - Start working on a task: {"command": "update_todo", "id": "abc-123", "status": "in_progress"}
/// - Complete a task: {"command": "update_todo", "id": "abc-123", "status": "completed"}
/// - Delete a todo: {"command": "delete_todo", "id": "abc-123"}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct TodoToolInput {
    /// The command to execute: "read_todos", "add_todo", "update_todo", or "delete_todo"
    pub command: String,
    /// Text for the todo (required for add_todo)
    #[serde(default)]
    pub text: Option<String>,
    /// Todo ID (required for update_todo and delete_todo)
    #[serde(default)]
    pub id: Option<String>,
    /// Status: "pending", "in_progress", or "completed" (for update_todo)
    #[serde(default)]
    pub status: Option<String>,
    /// Priority: "low", "medium", or "high" (for add_todo and update_todo)
    #[serde(default)]
    pub priority: Option<String>,
}

pub struct TodoTool {
    thread: WeakEntity<Thread>,
    acp_thread: WeakEntity<AcpThread>,
}

impl TodoTool {
    pub fn new(thread: WeakEntity<Thread>, acp_thread: WeakEntity<AcpThread>) -> Self {
        Self { thread, acp_thread }
    }
}

impl AgentTool for TodoTool {
    type Input = TodoToolInput;
    type Output = LanguageModelToolResultContent;

    const NAME: &'static str = "todo";

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Other
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        match input {
            Ok(i) => match i.command.as_str() {
                "read_todos" => "Read todos".into(),
                "add_todo" => "Add todo".into(),
                "update_todo" => "Update todo".into(),
                "delete_todo" => "Delete todo".into(),
                _ => "Manage todos".into(),
            },
            Err(_) => "Manage todos".into(),
        }
    }

    fn run(
        self: Arc<Self>,
        input: Self::Input,
        _event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output>> {
        let thread = self.thread.clone();
        let acp_thread = self.acp_thread.clone();
        cx.spawn(async move |cx| match input.command.as_str() {
            "read_todos" => {
                log::info!("Todo tool: reading todos");
                let todos =
                    thread.read_with(cx, |thread: &Thread, _| format_todos(&thread.todos))?;
                Ok(todos.into())
            }
            "add_todo" => {
                let text = input
                    .text
                    .ok_or_else(|| anyhow::anyhow!("text is required for add_todo"))?;
                log::info!("Todo tool: adding todo - {}", text);
                let priority = input
                    .priority
                    .as_ref()
                    .and_then(|p| parse_priority(p))
                    .unwrap_or(TodoPriority::Medium);
                let id = Uuid::new_v4().to_string();
                let todo = Todo {
                    id,
                    text,
                    status: TodoStatus::Pending,
                    priority,
                    created_at: Utc::now(),
                };
                thread.update(cx, |thread: &mut Thread, _| {
                    thread.todos.push(todo);
                })?;
                sync_plan(&thread, &acp_thread, cx)?;
                let todos =
                    thread.read_with(cx, |thread: &Thread, _| format_todos(&thread.todos))?;
                Ok(todos.into())
            }
            "update_todo" => {
                let id = input
                    .id
                    .ok_or_else(|| anyhow::anyhow!("id is required for update_todo"))?;
                log::info!("Todo tool: updating todo id={}", id);
                let mut found = false;
                thread.update(cx, |thread: &mut Thread, _| {
                    if let Some(todo) = thread.todos.iter_mut().find(|t| t.id == id) {
                        if let Some(status) = &input.status {
                            if let Some(s) = parse_status(status) {
                                todo.status = s;
                            }
                        }
                        if let Some(priority) = &input.priority {
                            if let Some(p) = parse_priority(priority) {
                                todo.priority = p;
                            }
                        }
                        if let Some(text) = &input.text {
                            todo.text = text.clone();
                        }
                        found = true;
                    }
                })?;
                sync_plan(&thread, &acp_thread, cx)?;
                let todos =
                    thread.read_with(cx, |thread: &Thread, _| format_todos(&thread.todos))?;
                if found {
                    Ok(todos.into())
                } else {
                    Ok(format!("Todo with id {} not found", id).into())
                }
            }
            "delete_todo" => {
                let delete_id = input
                    .id
                    .ok_or_else(|| anyhow::anyhow!("id is required for delete_todo"))?;
                log::info!("Todo tool: deleting todo id={}", delete_id);
                let mut found = false;
                thread.update(cx, |thread: &mut Thread, _| {
                    let initial_len = thread.todos.len();
                    thread.todos.retain(|t| t.id != delete_id);
                    found = thread.todos.len() < initial_len;
                })?;
                sync_plan(&thread, &acp_thread, cx)?;
                let todos =
                    thread.read_with(cx, |thread: &Thread, _| format_todos(&thread.todos))?;
                if found {
                    Ok(todos.into())
                } else {
                    Ok(format!("Todo with id {} not found", delete_id).into())
                }
            }
            _ => Ok(format!(
                "Unknown command: {}. Use: read_todos, add_todo, update_todo, or delete_todo",
                input.command
            )
            .into()),
        })
    }
}

fn sync_plan(
    thread: &WeakEntity<Thread>,
    acp_thread: &WeakEntity<AcpThread>,
    cx: &mut gpui::AsyncApp,
) -> Result<()> {
    let todos = thread.read_with(cx, |thread: &Thread, _| thread.todos.clone())?;
    let plan_entries: Vec<PlanEntry> = todos
        .iter()
        .map(|todo| {
            let content = cx.new(|cx| Markdown::new(todo.text.clone().into(), None, None, cx));
            let priority = match todo.priority {
                TodoPriority::Low => acp::PlanEntryPriority::Low,
                TodoPriority::Medium => acp::PlanEntryPriority::Medium,
                TodoPriority::High => acp::PlanEntryPriority::High,
            };
            let status = match todo.status {
                TodoStatus::Pending => acp::PlanEntryStatus::Pending,
                TodoStatus::InProgress => acp::PlanEntryStatus::InProgress,
                TodoStatus::Completed => acp::PlanEntryStatus::Completed,
            };
            PlanEntry {
                content,
                priority,
                status,
            }
        })
        .collect();
    acp_thread.update(cx, |thread: &mut AcpThread, cx| {
        thread.set_plan(plan_entries, cx);
    })?;
    Ok(())
}

fn parse_status(s: &str) -> Option<TodoStatus> {
    match s.to_lowercase().as_str() {
        "pending" => Some(TodoStatus::Pending),
        "in_progress" => Some(TodoStatus::InProgress),
        "completed" => Some(TodoStatus::Completed),
        _ => None,
    }
}

fn parse_priority(p: &str) -> Option<TodoPriority> {
    match p.to_lowercase().as_str() {
        "low" => Some(TodoPriority::Low),
        "medium" => Some(TodoPriority::Medium),
        "high" => Some(TodoPriority::High),
        _ => None,
    }
}

fn format_todos(todos: &[Todo]) -> String {
    if todos.is_empty() {
        return "No todos yet. Use the todo tool with command: 'add_todo' to create one."
            .to_string();
    }

    let mut output = String::from("# Todos\n\n");

    let mut completed: Vec<&Todo> = todos
        .iter()
        .filter(|t| matches!(t.status, TodoStatus::Completed))
        .collect();
    let mut in_progress: Vec<&Todo> = todos
        .iter()
        .filter(|t| matches!(t.status, TodoStatus::InProgress))
        .collect();
    let mut pending: Vec<&Todo> = todos
        .iter()
        .filter(|t| matches!(t.status, TodoStatus::Pending))
        .collect();

    completed.sort_by(|a, b| b.priority.cmp(&a.priority));
    in_progress.sort_by(|a, b| b.priority.cmp(&a.priority));
    pending.sort_by(|a, b| b.priority.cmp(&a.priority));

    if !completed.is_empty() {
        output.push_str("## Completed\n\n");
        for todo in &completed {
            output.push_str(&format!(
                "- [x] [{}] {} - `{}`\n",
                priority_to_string(&todo.priority),
                todo.text,
                todo.id
            ));
        }
        output.push('\n');
    }

    if !in_progress.is_empty() {
        output.push_str("## In Progress\n\n");
        for todo in &in_progress {
            output.push_str(&format!(
                "- 🏃 [{}] {} - `{}`\n",
                priority_to_string(&todo.priority),
                todo.text,
                todo.id
            ));
        }
        output.push('\n');
    }

    if !pending.is_empty() {
        output.push_str("## Pending\n\n");
        for todo in &pending {
            output.push_str(&format!(
                "- [ ] [{}] {} - `{}`\n",
                priority_to_string(&todo.priority),
                todo.text,
                todo.id
            ));
        }
    }

    output
}

fn priority_to_string(priority: &TodoPriority) -> &'static str {
    match priority {
        TodoPriority::Low => "low",
        TodoPriority::Medium => "medium",
        TodoPriority::High => "high",
    }
}
