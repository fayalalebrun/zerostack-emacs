use crate::agent::tools::WriteTodoList;
use crate::agent::tools::todo::{TODO_LIST, TodoItem, TodoWriteArgs};
use compact_str::CompactString;
use rig::tool::Tool;

fn item(content: &str, status: &str, priority: &str) -> TodoItem {
    TodoItem {
        content: content.to_string(),
        status: CompactString::new(status),
        priority: CompactString::new(priority),
    }
}

fn reset_todo_list() {
    let mut list = TODO_LIST
        .lock()
        .unwrap_or_else(|e: std::sync::PoisonError<_>| e.into_inner());
    list.clear();
}

#[tokio::test]
async fn definition_name() {
    let tool = WriteTodoList::new(None, None);
    let def = tool.definition(String::new()).await;
    assert_eq!(def.name, "todo_write");
}

#[tokio::test]
async fn definition_description_non_empty() {
    let tool = WriteTodoList::new(None, None);
    let def = tool.definition(String::new()).await;
    assert!(!def.description.is_empty());
}

#[tokio::test]
async fn definition_parameters_has_required_fields() {
    let tool = WriteTodoList::new(None, None);
    let def = tool.definition(String::new()).await;
    let params = def.parameters.as_object().unwrap();
    assert!(params.contains_key("properties"));
    let props = params["properties"].as_object().unwrap();
    assert!(props.contains_key("todos"));
}

#[tokio::test]
async fn call_with_empty_todos() {
    reset_todo_list();
    let tool = WriteTodoList::new(None, None);
    let args = TodoWriteArgs { todos: vec![] };
    let result = tool.call(args).await;
    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.contains("cleared"), "got: {}", output);
}

#[tokio::test]
async fn call_formats_todo_items_with_icons() {
    reset_todo_list();
    let tool = WriteTodoList::new(None, None);
    let args = TodoWriteArgs {
        todos: vec![
            item("High priority task", "high", "high"),
            item("Completed task", "completed", "medium"),
            item("In progress task", "in_progress", "medium"),
            item("Cancelled task", "cancelled", "low"),
            item("Low priority task", "low", "low"),
        ],
    };
    let result = tool.call(args).await;
    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.contains("[x]"));
    assert!(output.contains("[>]"));
    assert!(output.contains("[-]"));
    assert!(output.contains("[ ]"));
    assert!(output.contains("High priority task"));
    assert!(output.contains("Completed task"));
    assert!(output.contains("In progress task"));
    assert!(output.contains("Cancelled task"));
    assert!(output.contains("Low priority task"));
    assert!(output.contains("5 items"));
}

#[tokio::test]
async fn call_updates_global_todo_list() {
    reset_todo_list();
    let tool = WriteTodoList::new(None, None);
    let args = TodoWriteArgs {
        todos: vec![
            item("Task 1", "pending", "high"),
            item("Task 2", "pending", "medium"),
        ],
    };
    let _ = tool.call(args).await;

    let list = TODO_LIST
        .lock()
        .unwrap_or_else(|e: std::sync::PoisonError<_>| e.into_inner());
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].content, "Task 1");
    assert_eq!(list[1].content, "Task 2");
}

#[tokio::test]
async fn call_overwrites_previous_list() {
    reset_todo_list();
    let tool = WriteTodoList::new(None, None);

    let args1 = TodoWriteArgs {
        todos: vec![item("First", "pending", "high")],
    };
    let _ = tool.call(args1).await;

    let args2 = TodoWriteArgs {
        todos: vec![item("Second", "completed", "low")],
    };
    let _ = tool.call(args2).await;

    let list = TODO_LIST
        .lock()
        .unwrap_or_else(|e: std::sync::PoisonError<_>| e.into_inner());
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].content, "Second");
}
