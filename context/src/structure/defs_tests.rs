use super::*;
use crate::types::SymbolKind as K;

fn def(line: &str, lang: Lang) -> Option<(String, SymbolKind)> {
    detect(line, lang).map(|d| (d.name, d.kind))
}

fn named(name: &str, kind: SymbolKind) -> Option<(String, SymbolKind)> {
    Some((name.to_string(), kind))
}

#[test]
fn rust_definitions() {
    let r = Lang::Rust;
    assert_eq!(
        def("pub fn search(&self) {", r),
        named("search", K::Function)
    );
    assert_eq!(
        def("    pub(crate) async fn run() {", r),
        named("run", K::Function)
    );
    assert_eq!(
        def("pub const fn new() -> Self {", r),
        named("new", K::Function)
    );
    assert_eq!(def("extern \"C\" fn cb() {}", r), named("cb", K::Function));
    assert_eq!(def("pub struct Index {", r), named("Index", K::Struct));
    assert_eq!(def("enum Mode {", r), named("Mode", K::Enum));
    assert_eq!(def("pub trait Store {", r), named("Store", K::Interface));
    assert_eq!(
        def("pub type Result<T> = x;", r),
        named("Result", K::TypeAlias)
    );
    assert_eq!(def("mod tests {", r), named("tests", K::Module));
    assert_eq!(def("const MAX: usize = 3;", r), named("MAX", K::Constant));
    assert_eq!(def("macro_rules! bail {", r), named("bail", K::Macro));
    assert_eq!(def("let x = foo();", r), None);
    assert_eq!(def("// fn commented() {}", r), None);
}

#[test]
fn rust_impl_headers_with_nested_generics() {
    let d = detect(
        "impl<T: Into<String>> From<T> for crate::Key<T> {",
        Lang::Rust,
    )
    .unwrap();
    assert_eq!(d.name, "Key");
    assert_eq!(d.kind, K::Impl);
    assert_eq!(d.implements.as_deref(), Some("From"));
    let d = detect("impl Index {", Lang::Rust).unwrap();
    assert_eq!((d.name.as_str(), d.implements), ("Index", None));
    let d = detect("unsafe impl Send for Index {}", Lang::Rust).unwrap();
    assert_eq!(d.implements.as_deref(), Some("Send"));
    assert!(detect("implementation detail", Lang::Rust).is_none());
}

#[test]
fn python_ruby_lua_shell() {
    assert_eq!(
        def("async def fetch(url):", Lang::Python),
        named("fetch", K::Function)
    );
    assert_eq!(
        def("class Router(Base):", Lang::Python),
        named("Router", K::Class)
    );
    assert_eq!(
        def("  def valid?", Lang::Ruby),
        named("valid?", K::Function)
    );
    assert_eq!(
        def("module Auth::Tokens", Lang::Ruby),
        named("Tokens", K::Module)
    );
    assert_eq!(
        def("local function helper(x)", Lang::Lua),
        named("helper", K::Function)
    );
    assert_eq!(
        def("function M.render(x)", Lang::Lua),
        named("render", K::Function)
    );
    assert_eq!(def("deploy() {", Lang::Shell), named("deploy", K::Function));
    assert_eq!(
        def("function build_all {", Lang::Shell),
        named("build_all", K::Function)
    );
}

#[test]
fn javascript_and_typescript() {
    let js = Lang::TypeScript;
    assert_eq!(
        def("export async function load(id) {", js),
        named("load", K::Function)
    );
    assert_eq!(
        def("export default class App {", js),
        named("App", K::Class)
    );
    assert_eq!(def("interface Props {", js), named("Props", K::Interface));
    assert_eq!(
        def("export type Id = string;", js),
        named("Id", K::TypeAlias)
    );
    assert_eq!(
        def("const handler = async (req) => {", js),
        named("handler", K::Function)
    );
    assert_eq!(def("  render(props) {", js), named("render", K::Method));
    assert_eq!(def("  if (ready) {", js), None);
    assert_eq!(def("const x = 5;", js), None);
}

#[test]
fn go_definitions() {
    assert_eq!(
        def("func (s *Server) Serve() error {", Lang::Go),
        named("Serve", K::Method)
    );
    assert_eq!(def("func main() {", Lang::Go), named("main", K::Function));
    assert_eq!(
        def("type Config struct {", Lang::Go),
        named("Config", K::Struct)
    );
    assert_eq!(
        def("type Store interface {", Lang::Go),
        named("Store", K::Interface)
    );
}

#[test]
fn jvm_family() {
    assert_eq!(
        def("public final class TokenService {", Lang::Java),
        named("TokenService", K::Class)
    );
    assert_eq!(
        def("    public static void main(String[] args) {", Lang::Java),
        named("main", K::Method)
    );
    assert_eq!(
        def(
            "    private final Map<String, Integer> counts = new HashMap<>();",
            Lang::Java
        ),
        None
    );
    assert_eq!(
        def("data class User(val id: Int)", Lang::Kotlin),
        named("User", K::Class)
    );
    assert_eq!(
        def("suspend fun load(): Int {", Lang::Kotlin),
        named("load", K::Function)
    );
    assert_eq!(
        def("protocol Cache {", Lang::Swift),
        named("Cache", K::Interface)
    );
    assert_eq!(
        def("extension Array {", Lang::Swift),
        named("Array", K::Impl)
    );
}

#[test]
fn c_family() {
    assert_eq!(
        def("int main(int argc, char **argv) {", Lang::C),
        named("main", K::Function)
    );
    assert_eq!(
        def("static void *worker(void *arg)", Lang::C),
        named("worker", K::Function)
    );
    assert_eq!(def("struct node {", Lang::C), named("node", K::Struct));
    assert_eq!(
        def("#define MAX_LEN 64", Lang::C),
        named("MAX_LEN", K::Macro)
    );
    assert_eq!(
        def("void Parser::reset() {", Lang::Cpp),
        named("reset", K::Method)
    );
    assert_eq!(def("CALL_MACRO(x);", Lang::C), None);
    assert_eq!(def("    return compute(x);", Lang::C), None);
    assert_eq!(def("if (x) {", Lang::C), None);
}

#[test]
fn sql_is_case_insensitive() {
    assert_eq!(
        def("CREATE TABLE IF NOT EXISTS users (", Lang::Sql),
        named("users", K::Schema)
    );
    assert_eq!(
        def("create or replace view public.active_users as", Lang::Sql),
        named("active_users", K::Schema)
    );
}

#[test]
fn prose_languages_have_no_rules() {
    assert_eq!(def("fn looks_like_code() {", Lang::Markdown), None);
}
