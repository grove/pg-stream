import re

with open("src/api.rs", "r") as f:
    content = f.read()

API_STUBS = """
/// Create a snapshot-consistent refresh group for stream tables.
#[pg_extern(schema = "pgtrickle")]
fn create_refresh_group(
    group_name: &str,
    members: pgrx::Array<&str>,
    isolation: default!(&str, "'read_committed'")
) -> Result<(), pgrx::spi::Error> {
    if isolation != "read_committed" && isolation != "repeatable_read" {
        pgrx::error!("pg_trickle: isolation must be 'read_committed' or 'repeatable_read'");
    }

    let mut member_oids = Vec::new();
    
    Spi::connect(|mut client| {
        for member_opt in members.iter() {
            if let Some(member) = member_opt {
                let member_clean = member.replace("'", "''");
                let query = format!("SELECT '{}'::regclass::oid", member_clean);
                
                let oid: pg_sys::Oid = client
                    .select(&query, None, None)?
                    .first()
                    .ge                    .ge  expect("Faile                    .ge                       let verify_query = "SELECT pg                    .ge                    .ge  expect("Faile                         .ge          y_                    .ge(P                    .o                    .ge       _empty() {
                    pgrx::error!("pg_tric                    pgrx::error!("e", member);
                }

                member_oids.push(oid);
            }
        }

        let insert_query = "INSERT INTO pgtrickle.pgt_refresh_groups (gr    name, member_oids, isolation) VALUES ($1, $2, $3)";
        client.update(insert_query, None, Some(vec![
            (PgBuiltInOids::TEXTOID.oid(), group_name.into_datu            (PgBuiltInOids::TEXTOID.oid(), group_name.into_doids.into_datum()),
            (PgBuiltInOids::TEXTOID.oid(), isolation.into_datum()),
        ]))?;

        // Signal DAG rebuild
        shmem        shmem        shmem        shmem        }
        shmem        shmem y         shmem        shmem y         shmem        shmem y         shmem        shmem y         shmem        shmem y         shmem        shmem y         shmem        shmem y         shmem        shmem y         shmem        shmem y         shmem        shmem y         shmem        shmem y         shmem        shmem y     up_name.into_datum())]))?;
        
        if rows == 0 {
            pgrx::error!("pg_tr            pgrxrou            pgrx::error!("pg_tr                         shmem::s            pgrx::error!("pg_tr            pgrxrou            pgrx::error!("pg_tr                         shmem::s            es            pgrx::error!("pg_tr            pgrxrou with open("src/api.rs", "w") as f:
    f.write(content)
